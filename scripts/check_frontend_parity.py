#!/usr/bin/env python3
"""Offline SDK/CLI JSON/RPC/Web integration driver. Only loopback HTTP and fake credentials.
Build first: cargo build --locked -p aicoder-cli -p aicoder-web --features server --bins --examples
"""
import argparse
import http.client
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
parser.add_argument("--bin-dir", type=Path, default=ROOT / "target/debug")
args = parser.parse_args()
bin_dir = args.bin_dir.resolve()
TOKEN = "fixture-web-token-0123456789abcdef"


class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert "parity" in json.dumps(request["messages"])
        body = json.dumps({"id": "fixture", "object": "chat.completion", "created": 1,
                           "model": "test", "choices": [{"index": 0, "message": {
                               "role": "assistant", "content": "same answer"}, "finish_reason": "stop"}],
                           "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def line(pipe):
    # Frame waits have a deadline; buffered read-ahead is avoided by using unbuffered pipes.
    with selectors.DefaultSelector() as selector:
        selector.register(pipe, selectors.EVENT_READ)
        data = bytearray()
        while not data.endswith(b"\n"):
            if not selector.select(10):
                raise RuntimeError("Frontend frame deadline")
            byte = os.read(pipe.fileno(), 1)
            if not byte or len(data) >= 8 * 1024 * 1024:
                raise RuntimeError("Incomplete or oversized frontend frame")
            data.extend(byte)
        return bytes(data)


def normalize(events):
    text = "".join(e["event"]["delta"] for e in events if e["event"]["type"] == "content_chunk")
    terminal = next(e["event"] for e in events if e["event"]["type"] == "agent_completed")
    return {"text": text, "rounds": terminal["rounds"], "usage": terminal["usage"]}


provider = http.server.HTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=provider.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix="aicoder-parity-") as workspace:
        env = {"PATH": os.environ.get("PATH", ""), "AICODER_HOME": workspace + "/state",
               "AICODER_API_BASE": f"http://127.0.0.1:{provider.server_address[1]}/v1",
               "OPENAI_API_KEY": "fixture-key", "AICODER_MODEL": "test", "AICODER_STREAM": "false",
               "AICODER_PROXY": "disabled", "AICODER_MAX_RETRIES": "0", "AICODER_WEB_TOKEN": TOKEN}
        common = ["--no-config", "--no-resources", "--no-session", "--workspace", workspace]
        sdk = json.loads(subprocess.check_output([str(bin_dir / "examples/provider_turn"), workspace], env=env, timeout=15))
        cli = subprocess.check_output([str(bin_dir / "aicoder"), *common, "--json", "--prompt", "parity"], env=env, timeout=15)
        assert normalize([json.loads(v) for v in cli.splitlines()]) == sdk
        rpc = subprocess.Popen([str(bin_dir / "aicoder"), *common, "--rpc"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, bufsize=0)
        try:
            rpc.stdin.write(b'{"version":1,"id":1,"command":{"type":"prompt","text":"parity"}}\n')
            events = []
            while True:
                frame = json.loads(line(rpc.stdout))
                if frame["type"] == "event":
                    events.append(frame["event"])
                if frame["type"] == "run_finished":
                    assert frame["result"]["status"] == "completed"
                    break
            assert normalize(events) == sdk
            rpc.stdin.write(b'{"version":1,"id":2,"command":{"type":"shutdown"}}\n')
            assert json.loads(line(rpc.stdout))["ok"]
            assert rpc.wait(timeout=5) == 0
        finally:
            if rpc.poll() is None:
                rpc.kill()
                rpc.wait()
        web = subprocess.Popen([str(bin_dir / "aicoder-web"), *common, "--listen", "127.0.0.1:0"], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        try:
            ready = line(web.stderr).decode().strip()
            assert ready.startswith("Web example listening at http://"), ready
            address = ready.removeprefix("Web example listening at http://")
            connection = http.client.HTTPConnection(address, timeout=10)
            headers = {"Authorization": "Bearer " + TOKEN, "Content-Type": "application/json"}
            connection.request("POST", "/api/runs", json.dumps({"id": 1, "text": "parity"}), headers)
            response = connection.getresponse()
            assert response.status == 202
            accepted = json.loads(response.read())
            connection.request("GET", accepted["events"], headers=headers)
            response = connection.getresponse()
            assert response.status == 200
            events = []
            for block in response.read().decode().split("\n\n"):
                kind = next((v[6:].strip() for v in block.splitlines() if v.startswith("event:")), "")
                data = "\n".join(v[5:].lstrip() for v in block.splitlines() if v.startswith("data:"))
                if kind == "agent":
                    events.append(json.loads(data))
                if kind == "result":
                    assert json.loads(data)["status"] == "completed"
            assert normalize(events) == sdk
            connection.close()
            web.terminate()
            assert web.wait(timeout=8) == 0
        finally:
            if web.poll() is None:
                web.kill()
                web.wait()
        print(json.dumps({"frontends": ["sdk", "cli_json", "rpc", "web_sse"], "normalized": sdk}, sort_keys=True))
finally:
    provider.shutdown()
    provider.server_close()
