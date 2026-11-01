#!/usr/bin/env python3
"""Real CLI/RPC JS composition with loopback-only model, fake credentials and temp files.
Build first: cargo build --locked -p aicoder-cli --features code-mode
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import selectors
import subprocess
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/aicoder")
args = parser.parse_args()
requests = []
failures = []
CODE = """
const reads = await Promise.allSettled([
  tools.read_file({path:'one.txt'}), tools.read_file({path:'two.txt'})
]);
const selected = reads.map(r => r.value.content.split('\\n')[0]);
await tools.write_file({path:'result.txt', content:selected.join(',')});
text(selected);
"""


class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            assert any(t["function"]["name"] == "codemode" for t in request["tools"])
            call = request["messages"][-1]["role"] == "user"
            if call:
                message = {"role": "assistant", "content": None, "tool_calls": [
                    {"id": "program", "type": "function", "function": {
                        "name": "codemode", "arguments": json.dumps({"code": CODE})}}]}
            else:
                assert "PRIVATE_LOTS_" not in json.dumps(request["messages"])
                body = json.loads(request["messages"][-1]["content"])
                assert body["ok"], body
                assert body["output"]["program"]["items"] == [["alpha", "beta"]], body
                message = {"role": "assistant", "content": "composition verified"}
            body = json.dumps({"id": "fixture", "object": "chat.completion", "created": 1,
                               "model": "test", "choices": [{"index": 0, "message": message,
                               "finish_reason": "tool_calls" if call else "stop"}],
                               "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except Exception as error:
            failures.append(repr(error))
            self.send_error(500)

    def log_message(self, *_):
        pass


def line(pipe):
    with selectors.DefaultSelector() as selector:
        selector.register(pipe, selectors.EVENT_READ)
        data = bytearray()
        while not data.endswith(b"\n"):
            if not selector.select(10):
                raise RuntimeError("RPC frame deadline")
            byte = os.read(pipe.fileno(), 1)
            if not byte or len(data) >= 8 * 1024 * 1024:
                raise RuntimeError("Incomplete or oversized frame")
            data.extend(byte)
        return json.loads(data)


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix="aicoder-code-mode-") as folder:
        root = Path(folder)
        (root / "one.txt").write_text("alpha\n" + "PRIVATE_LOTS_" * 128)
        (root / "two.txt").write_text("beta\n" + "PRIVATE_LOTS_" * 128)
        env = {"PATH": os.environ.get("PATH", ""), "AICODER_HOME": str(root / "state"),
               "AICODER_API_BASE": f"http://127.0.0.1:{server.server_address[1]}/v1",
               "OPENAI_API_KEY": "fixture-key", "AICODER_MODEL": "test", "AICODER_STREAM": "false",
               "AICODER_PROXY": "disabled", "AICODER_MAX_RETRIES": "0"}
        common = [str(args.binary.resolve()), "--no-config", "--no-resources", "--no-session",
                  "--workspace", str(root), "--code-mode"]
        output = subprocess.check_output([*common, "--json", "--yes", "--prompt", "compose"], env=env, timeout=15)
        events = [json.loads(raw)["event"] for raw in output.splitlines()]
        ended = next(e for e in events if e["type"] == "agent_completed")
        assert ended["rounds"] == 2, ended
        assert sum(e["type"] == "tool_execution_started" for e in events) == 4, events
        assert (root / "result.txt").read_text() == "alpha,beta"
        (root / "result.txt").unlink()
        rpc = subprocess.Popen([*common, "--rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0)
        sequence = 0
        approvals = []
        def send(command):
            global sequence
            sequence += 1
            rpc.stdin.write((json.dumps({"version": 1, "id": sequence, "command": command}) + "\n").encode())
        try:
            send({"type": "prompt", "text": "compose"})
            while True:
                frame = line(rpc.stdout)
                if frame["type"] == "response":
                    assert frame["ok"], frame
                if frame["type"] == "observation_error":
                    raise RuntimeError(frame)
                if frame["type"] == "event" and frame["event"]["event"]["type"] == "run_approval_requested":
                    question = frame["event"]["event"]["request"]
                    approvals.append(question["invocation"]["name"])
                    if approvals[-1] == "write_file":
                        assert not (root / "result.txt").exists()
                        assert question["invocation"]["preview"]
                    send({"type": "approve", "run_id": question["id"]["run_id"],
                          "approval_id": question["id"], "decision": "allow_once"})
                if frame["type"] == "run_finished":
                    assert frame["result"]["status"] == "completed", frame
                    assert frame["result"]["tool_calls"] == 4, frame
                    break
            assert approvals == ["codemode", "write_file"], approvals
            assert (root / "result.txt").read_text() == "alpha,beta"
            send({"type": "shutdown"})
            while True:
                frame = line(rpc.stdout)
                if frame["type"] == "response" and frame["id"] == sequence:
                    assert frame["ok"], frame
                    break
            assert rpc.wait(timeout=5) == 0
        finally:
            if rpc.poll() is None:
                rpc.kill()
                rpc.wait()
            rpc.stdin.close()
            rpc.stdout.close()
        assert not failures, failures
        assert len(requests) == 4, len(requests)
        print(json.dumps({"frontends": ["cli_json", "rpc"], "actual_js": True,
                          "tool_calls_per_run": 4, "rpc_approvals": approvals,
                          "filtered_model_view": True, "loopback_model_requests": len(requests)}))
finally:
    server.shutdown()
    server.server_close()
