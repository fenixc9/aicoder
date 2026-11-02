#!/usr/bin/env python3
"""Offline benchmark CLI, archive, comparison and loopback adapter regression. Fake credentials only.
Build first: cargo build --locked -p aicoder-eval --bin aicoder-bench
"""
import argparse
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/aicoder-bench")
parser.add_argument("--output", type=Path)
args = parser.parse_args()
requests = []
failures = []


def invoke(arguments, env, expected=0):
    done = subprocess.run([str(args.binary.resolve()), *arguments], cwd=ROOT, env=env,
                          capture_output=True, text=True, timeout=90)
    assert done.returncode == expected, (arguments, done.returncode, done.stdout, done.stderr)
    return json.loads(done.stdout)


class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(request)
            text = json.dumps(request["messages"])
            assert "hidden_coding_tests" not in text and "candidate/src/lib.rs" not in text
            assert request.get("seed") == 0
            tools = [b for b in request["messages"] if b["role"] == "tool"]
            if len(tools) < 2:
                name = "read_file" if not tools else "write_file"
                arguments = {"path": "config.json"}
                if tools:
                    arguments["content"] = '{"retry":{"attempts":3,"backoff_ms":100},"telemetry":false}\n'
                message = {"role": "assistant", "content": None, "tool_calls": [{
                    "id": f"call-{len(tools)}", "type": "function", "function": {
                        "name": name, "arguments": json.dumps(arguments)}}]}
                stop = "tool_calls"
            else:
                message = {"role": "assistant", "content": "updated"}
                stop = "stop"
            body = json.dumps({"id": "fixture", "object": "chat.completion", "created": 1,
                               "model": request["model"], "choices": [{"index": 0,
                               "message": message, "finish_reason": stop}],
                               "usage": {"prompt_tokens": 4, "completion_tokens": 1, "total_tokens": 5}}).encode()
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


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    temporary = None
    if args.output:
        output = args.output.resolve()
        output.mkdir(parents=True, exist_ok=False)
    else:
        temporary = tempfile.TemporaryDirectory(prefix="aicoder-benchmark-check-")
        output = Path(temporary.name)
    try:
        env = {"PATH": os.environ.get("PATH", ""), "AICODER_API_BASE": "not-used-offline",
               "OPENAI_API_KEY": "fixture-value-must-not-enter-report"}
        for key in ["RUSTUP_HOME", "CARGO_HOME", "RUSTUP_TOOLCHAIN"]:
            if key in os.environ:
                env[key] = os.environ[key]
        listing = invoke(["list"], env)
        assert len(listing["cases"]) == 4 and "grading" not in json.dumps(listing)
        left = output / "left"
        right = output / "right"
        summaries = []
        for directory in [left, right]:
            summary = invoke(["run", "--output", str(directory)], env)
            assert summary["passed"] == summary["selected"] == 4
            assert summary["total_tool_calls"] == 8 and summary["cost"] is None
            summaries.append(summary)
            checked = invoke(["verify", str(directory)], env)
            assert checked["verified"] and checked["summary"] == summary
            assert "fixture-value-must-not-enter-report" not in (directory / "manifest.json").read_text()
            patch = directory / "cases/inclusive_range-0/workspace.patch"
            assert "+pub fn contains" in patch.read_text()
        replay = invoke(["replay", str(left), "--case", "inclusive_range"], env)
        assert replay["observation_only"] and replay["trajectory"]["tool_calls"] == 2
        compared = invoke(["compare", str(left), str(right)], env)
        assert compared["both_passed"] == 4 and not compared["regressions"]
        for variant in ["noop", "provider-error", "limit"]:
            directory = output / variant
            summary = invoke(["run", "--output", str(directory), "--variant", variant], env, expected=1)
            assert summary["selected"] == 4 and summary["passed"] == 0
            invoke(["verify", str(directory)], env)
        assert not requests, "offline benchmark contacted a service"
        network = dict(env, AICODER_API_BASE=f"http://127.0.0.1:{server.server_address[1]}/v1",
                       AICODER_PROXY="disabled", AICODER_MAX_RETRIES="0", AICODER_PROVIDER_ID="loopback-benchmark-fixture")
        live = output / "loopback"
        summary = invoke(["run", "--output", str(live), "--case", "retry_config",
                          "--live", "--model", "fixture"], network)
        assert summary["selected"] == summary["passed"] == 1 and summary["total_tokens"] == 15
        invoke(["verify", str(live)], network)
        assert len(requests) == 3 and not failures, failures
        result = {"cases_per_offline_run": 4, "scripted_repetitions": 2,
                  "failure_variants_in_denominator": ["noop", "provider-error", "limit"],
                  "verified_archives": 6, "replay_observation_only": True,
                  "same_inputs_comparison": True, "default_model_network_requests": 0,
                  "explicit_loopback_model_requests": len(requests)}
        (output / "check-summary.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result))
    finally:
        if temporary:
            temporary.cleanup()
finally:
    server.shutdown()
    server.server_close()
