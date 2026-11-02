#!/usr/bin/env python3
"""Offline stock-host stdio activation checks, using a real MCP child and loopback provider.

Build: cargo build --locked -p aicoder-cli -p aicoder-tui -p aicoder-web
       -p aicoder-runtime --features server,host-extensions,mcp-fixtures --bins
Linux PTY and /proc checks require no browser, model credentials or remote access.
"""
import argparse
import fcntl
import http.client
import http.server
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--bin-dir", type=Path, default=ROOT / "target/debug")
args = parser.parse_args()
BINS = args.bin_dir.resolve()
TOKEN = "fixture-host-token-0123456789abcdef"
requests = []
failures = []


class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            value = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(value)
            names = {t["function"]["name"] for t in value["tools"]}
            assert "host__write" in names and "host__read" in names, names
            assert "DO_NOT_MAKE_THIS_A_SYSTEM_PROMPT" not in json.dumps(value)
            call = value["messages"][-1]["role"] != "tool"
            message = {"role": "assistant", "content": None, "tool_calls": [{
                "id": "host-write", "type": "function", "function": {"name": "host__write",
                "arguments": json.dumps({"path": "result.txt", "content": "host approved"})}}]} if call else {
                "role": "assistant", "content": "host done"}
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
            failures.append(str(error))
            raise

    def log_message(self, *_):
        pass


def until(test, label):
    deadline = time.monotonic() + 12
    while time.monotonic() < deadline:
        result = test()
        if result:
            return result
        time.sleep(.01)
    raise RuntimeError(label + " deadline")


def line(pipe):
    data = bytearray()
    deadline = time.monotonic() + 12
    while not data.endswith(b"\n"):
        remaining = deadline - time.monotonic()
        if remaining <= 0 or not select.select([pipe], [], [], remaining)[0]:
            raise RuntimeError("frame deadline")
        chunk = os.read(pipe.fileno(), 1)
        if not chunk or len(data) >= 8 * 1024 * 1024:
            raise RuntimeError("incomplete/oversized frame")
        data.extend(chunk)
    return data


def stop(child):
    if child.poll() is None:
        child.kill()
        child.wait(timeout=5)


def reaped(root):
    pid = int((root / "mcp.pid").read_text())
    until(lambda: not Path(f"/proc/{pid}").exists(), "MCP child reap")


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix="aicoder-host-extensions-") as folder:
        for frontend in ["cli_json", "rpc", "web", "tui"]:
            root = Path(folder) / frontend
            root.mkdir()
            launch = root / "launch.json"
            launch.write_text(json.dumps({"schema_version": 1, "entries": [{"id": "host",
                "kind": "mcp_stdio", "program": str(BINS / "aicoder-mcp-fixture"),
                "arguments": ["modern", str(root / "mcp.pid")]}]}))
            env = {"PATH": os.environ.get("PATH", ""), "TERM": "xterm-256color",
                "AICODER_HOME": str(root / "state"), "OPENAI_API_KEY": "fixture-key",
                "AICODER_API_BASE": f"http://127.0.0.1:{server.server_address[1]}/v1",
                "AICODER_MODEL": "test", "AICODER_STREAM": "false",
                "AICODER_PROXY": "disabled", "AICODER_MAX_RETRIES": "0", "AICODER_WEB_TOKEN": TOKEN}
            common = ["--no-config", "--no-resources", "--workspace", str(root),
                "--extension-config", str(launch)]
            if frontend == "cli_json":
                output = subprocess.check_output([str(BINS / "aicoder"), *common,
                    "--no-session", "--yes", "--json", "--prompt", "host fixture"], env=env, timeout=15)
                events = [json.loads(raw)["event"] for raw in output.splitlines()]
                assert any(e["type"] == "agent_completed" for e in events)
                assert sum(e["type"] == "tool_execution_started" for e in events) == 1
            elif frontend == "rpc":
                child = subprocess.Popen([str(BINS / "aicoder"), *common, "--no-session", "--rpc"],
                    env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0)
                sequence = 0
                approvals = []

                def send(command):
                    global sequence
                    sequence += 1
                    child.stdin.write((json.dumps({"version": 1, "id": sequence, "command": command}) + "\n").encode())

                try:
                    send({"type": "prompt", "text": "host fixture"})
                    while True:
                        frame = json.loads(line(child.stdout))
                        if frame["type"] == "response":
                            assert frame["ok"], frame
                        if frame["type"] == "event" and frame["event"]["event"]["type"] == "run_approval_requested":
                            question = frame["event"]["event"]["request"]
                            assert not (root / "result.txt").exists()
                            approvals.append(question["invocation"]["name"])
                            send({"type": "approve", "run_id": question["id"]["run_id"],
                                "approval_id": question["id"], "decision": "allow_once"})
                        if frame["type"] == "run_finished":
                            assert frame["result"]["status"] == "completed", frame
                            break
                    assert approvals == ["host__write"], approvals
                    send({"type": "shutdown"})
                    while json.loads(line(child.stdout)).get("id") != sequence:
                        pass
                    assert child.wait(timeout=8) == 0
                finally:
                    stop(child)
                    child.stdin.close()
                    child.stdout.close()
            elif frontend == "web":
                child = subprocess.Popen([str(BINS / "aicoder-web"), *common,
                    "--no-session", "--listen", "127.0.0.1:0"], env=env,
                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, bufsize=0)
                try:
                    ready = line(child.stderr).decode().strip()
                    prefix = "Web example listening at http://"
                    assert ready.startswith(prefix), ready
                    connection = http.client.HTTPConnection(ready.removeprefix(prefix), timeout=10)

                    def api(method, path, payload=None):
                        connection.request(method, path, None if payload is None else json.dumps(payload),
                            {"Authorization": "Bearer " + TOKEN, "Content-Type": "application/json"})
                        response = connection.getresponse()
                        value = json.loads(response.read())
                        assert 200 <= response.status < 300, value
                        return value

                    accepted = api("POST", "/api/runs", {"id": 1, "text": "host fixture"})
                    path = "/api/runs/" + accepted["run_id"]
                    state = until(lambda: (s if (s := api("GET", path))["approvals"] else None), "Web approval")
                    assert not (root / "result.txt").exists()
                    question = state["approvals"][0]
                    assert question["invocation"]["name"] == "host__write"
                    api("POST", path + "/approval", {"approval_id": question["id"], "decision": "allow_once"})
                    state = until(lambda: (s if (s := api("GET", path))["result"] else None), "Web completion")
                    assert state["result"]["status"] == "completed", state
                    connection.close()
                    child.terminate()
                    assert child.wait(timeout=8) == 0
                finally:
                    stop(child)
                    child.stderr.close()
            else:
                master, slave = pty.openpty()
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 140, 0, 0))
                child = subprocess.Popen([str(BINS / "aicoder-tui"), *common], env=env,
                    stdin=slave, stdout=slave, stderr=slave, close_fds=True)
                os.close(slave)
                output = bytearray()

                def marker(text):
                    while select.select([master], [], [], 0)[0]:
                        try:
                            data = os.read(master, 65536)
                        except OSError:
                            break
                        if not data:
                            break
                        output.extend(data)
                        if len(output) > 1024 * 1024:
                            del output[:-1024 * 1024]
                    assert child.poll() is None, output[-2000:]
                    return text.encode() in output

                def recorded_completion():
                    marker("")  # Drain the real PTY; terminal diff rendering can split words.
                    records = [json.loads(raw) for file in (root / "state/sessions").glob("*.jsonl")
                        for raw in file.read_text().splitlines()]
                    entries = [e["data"] for record in records for e in record.get("entries", [])]
                    return any(e.get("kind") == "turn_end" for e in entries) and "host done" in json.dumps(entries)

                try:
                    until(lambda: marker("Sessions"), "TUI startup")
                    os.write(master, b"host fixture\r")
                    until(lambda: marker("host__write") and marker("Approval"), "TUI approval")
                    assert not (root / "result.txt").exists()
                    os.write(master, b"y")
                    until(recorded_completion, "TUI canonical completion")
                    # A journal commit can precede delivery of the owner's result to the UI.
                    time.sleep(.15)
                    marker("")
                    os.write(master, b"\x03")
                    assert child.wait(timeout=8) == 0
                finally:
                    stop(child)
                    os.close(master)
            assert (root / "result.txt").read_text() == "host approved", frontend
            reaped(root)
        for binary, extra, termination in [
            ("aicoder", ["--json", "--prompt", "never start"], signal.SIGINT),
            ("aicoder-tui", [], signal.SIGINT),
            ("aicoder-web", ["--no-session", "--listen", "127.0.0.1:0"], signal.SIGTERM),
        ]:
            root = Path(folder) / (binary + "-startup")
            root.mkdir()
            launch = root / "launch.json"
            launch.write_text(json.dumps({"schema_version": 1, "entries": [{"id": "slow",
                "kind": "process", "program": str(BINS / "aicoder-process-fixture"),
                "arguments": ["init_slow", str(root / "mcp.pid")]}]}))
            env["AICODER_HOME"] = str(root / "state")
            common = ["--no-config", "--no-resources", "--workspace", str(root),
                "--extension-config", str(launch)]
            if binary in ["aicoder", "aicoder-tui"]:
                subprocess.run([str(BINS / binary), *common, "--show-config"], env=env,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10, check=False)
                assert not (root / "mcp.pid").exists(), "metadata must never launch children"
            child = subprocess.Popen([str(BINS / binary), *common, *extra], env=env,
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                until(lambda: (root / "mcp.pid").exists(), "slow startup")
                child.send_signal(termination)
                assert child.wait(timeout=8) != 0, "cancelled startup must not report success"
                reaped(root)
            finally:
                stop(child)
        assert len(requests) == 8 and not failures, (len(requests), failures)
        print(json.dumps({"frontends": ["cli_json", "rpc", "web", "tui_pty"],
            "real_mcp_child": True, "explicit_approvals": ["rpc", "web", "tui"],
            "all_children_reaped": True, "startup_cancellation": ["cli", "tui", "web"],
            "metadata_does_not_launch": True, "loopback_model_requests": len(requests)}))
finally:
    server.shutdown()
    server.server_close()
