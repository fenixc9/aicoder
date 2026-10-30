#!/usr/bin/env python3
"""Small optional Python client example; normal agent binaries do not require Python.

Usage: python3 scripts/rpc_client.py 'inspect the project' target/release/aicoder --workspace .
Tool questions default to deny. The caller chooses provider/workspace settings on the CLI.
"""
import json
import subprocess
import sys

if len(sys.argv) < 3:
    raise SystemExit(__doc__)
next_id = 0
with subprocess.Popen([*sys.argv[2:], "--rpc"], stdin=subprocess.PIPE,
                      stdout=subprocess.PIPE) as child:
    def send(command):
        global next_id
        next_id += 1
        child.stdin.write((json.dumps({"version": 1, "id": next_id, "command": command}) + "\n").encode())
        child.stdin.flush()
        return next_id

    try:
        send({"type": "prompt", "text": sys.argv[1]})
        closing_id = None
        completed = False
        while line := child.stdout.readline(8 * 1024 * 1024 + 1):
            if len(line) > 8 * 1024 * 1024 or not line.endswith(b"\n"):
                raise RuntimeError("RPC frame exceeds client limit or is incomplete")
            frame = json.loads(line)
            if frame["version"] != 1:
                raise RuntimeError("Unsupported RPC version")
            if frame["type"] == "response" and not frame["ok"]:
                raise RuntimeError(frame["error"])
            if frame["type"] == "event":
                event = frame["event"]["event"]
                if event["type"] == "run_approval_requested":
                    request = event["request"]
                    send({"type": "approve", "run_id": request["id"]["run_id"],
                          "approval_id": request["id"], "decision": "deny"})
            if frame["type"] == "observation_error":
                print(json.dumps(frame), file=sys.stderr)
            if frame["type"] == "run_finished":
                completed = frame["result"]["status"] == "completed"
                print(json.dumps(frame["result"], ensure_ascii=False))
                closing_id = send({"type": "shutdown"})
            if closing_id is not None and frame["type"] == "response" and frame["id"] == closing_id:
                break
    finally:
        child.stdin.close()  # EOF asks the owner to cancel/await, even after client errors.
        child.stdout.close()
    try:
        status = child.wait(timeout=30)
    except subprocess.TimeoutExpired:
        child.kill()
        child.wait()
        raise
    if status or closing_id is None or not completed:
        raise SystemExit(status or 1)
