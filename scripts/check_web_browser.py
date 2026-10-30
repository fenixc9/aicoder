#!/usr/bin/env python3
"""Optional real-browser check; requires development-only Playwright + its Chromium.
No existing browser profile, external provider or real credential is used.
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

from playwright.sync_api import sync_playwright, expect

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/aicoder-web")
parser.add_argument("--screenshot", type=Path)
args = parser.parse_args()
TOKEN = "fixture-web-token-0123456789abcdef"


class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        value = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        last = value["messages"][-1]
        if last["role"] == "user" and "wait" in json.dumps(last["content"]):
            # Hold until cancellation closes the provider connection.
            self.connection.settimeout(10)
            self.connection.recv(1)
            return
        tool = last["role"] != "tool"
        message = {"role": "assistant", "content": None, "tool_calls": [{
            "id": "browser-write", "type": "function", "function": {"name": "write_file",
                "arguments": json.dumps({"path": "result.txt", "content": "browser approved"})}}]} if tool else {
                    "role": "assistant", "content": 'same answer <img src=x onerror="window.injected=true">'}
        body = json.dumps({"id": "fixture", "object": "chat.completion", "created": 1, "model": "test",
                           "choices": [{"index": 0, "message": message, "finish_reason": "tool_calls" if tool else "stop"}],
                           "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=provider.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix="aicoder-web-browser-") as workspace:
        env = {"PATH": os.environ.get("PATH", ""), "AICODER_HOME": workspace + "/state", "AICODER_WEB_TOKEN": TOKEN,
               "AICODER_API_BASE": f"http://127.0.0.1:{provider.server_address[1]}/v1", "AICODER_MODEL": "test",
               "AICODER_STREAM": "false", "AICODER_PROXY": "disabled", "OPENAI_API_KEY": "fixture-key"}
        host = subprocess.Popen([str(args.binary.resolve()), "--workspace", workspace, "--listen", "127.0.0.1:0",
                                 "--no-session", "--no-config", "--no-resources"], env=env,
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, bufsize=0)
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(host.stderr, selectors.EVENT_READ)
                if not selector.select(10):
                    raise RuntimeError("Web startup deadline")
                ready = host.stderr.readline().decode().strip()
            assert ready.startswith("Web example listening at http://"), ready
            url = ready.removeprefix("Web example listening at ")
            with sync_playwright() as p:
                browser = p.chromium.launch(headless=True, args=["--no-sandbox"])
                try:
                    page = browser.new_page(viewport={"width": 1100, "height": 1000})
                    page.set_default_timeout(10000)
                    errors = []
                    page.on("pageerror", lambda error: errors.append(str(error)))
                    page.goto(url)
                    page.locator("#token").fill(TOKEN)
                    page.locator("#connect").click()
                    page.locator("#connect-panel").wait_for(state="hidden")
                    page.locator("#prompt").fill("browser fixture")
                    page.locator("#submit").click()
                    page.locator("#approval").wait_for(state="visible")
                    assert "result.txt" in page.locator("#preview").inner_text()
                    assert "session_scope" in page.locator("#preview").inner_text()
                    page.locator("#once").click()
                    expect(page.locator("#status")).to_have_text("completed", timeout=10000)
                    expect(page.locator("#output")).to_contain_text("<img", timeout=10000)
                    assert Path(workspace, "result.txt").read_text() == "browser approved"
                    assert page.locator("#output img").count() == 0
                    if args.screenshot:
                        page.screenshot(path=str(args.screenshot), full_page=True)
                    page.locator("#prompt").fill("wait")
                    page.locator("#submit").click()
                    expect(page.locator("#cancel")).to_be_enabled(timeout=10000)
                    page.locator("#cancel").click()
                    expect(page.locator("#status")).to_have_text("aborted", timeout=10000)
                    assert not errors, errors
                    print(json.dumps({"browser": "chromium", "version": browser.version, "authenticated": True,
                                      "prepared_approval": True, "sse_result": True, "inert_model_html": True, "cancelled": True}))
                finally:
                    browser.close()
            host.terminate()
            assert host.wait(timeout=8) == 0
        finally:
            if host.poll() is None:
                host.kill()
                host.wait()
finally:
    provider.shutdown()
    provider.server_close()
