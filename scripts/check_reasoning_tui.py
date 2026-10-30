#!/usr/bin/env python3
"""Offline real-PTY reasoning controls, using only Python stdlib and a fake HTTP provider."""
import argparse
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/aicoder-tui")
args = parser.parse_args()
requests = []
class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        value = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        requests.append(value)
        if "wait-provider" in json.dumps(value["messages"][-1]["content"]):
            self.connection.settimeout(10); self.connection.recv(1); return
        body = json.dumps({"id":"fixture","object":"chat.completion","created":1,"model":"m1","choices":[{"index":0,"message":{"role":"assistant","content":f"done-{len(requests)}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}).encode()
        self.send_response(200); self.send_header("Content-Type","application/json"); self.send_header("Content-Length",str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self,*_): pass
server = http.server.ThreadingHTTPServer(("127.0.0.1",0),Provider)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix="aicoder-reasoning-pty-") as folder:
        root=Path(folder); workspace=root/"workspace"; workspace.mkdir(); state=root/"state"; state.mkdir()
        settings=root/"settings.json"
        settings.write_text(json.dumps({"version":1,"profiles":{"work":{"base_url":f"http://127.0.0.1:{server.server_address[1]}/v1","api_key_env":"FIXTURE_KEY","provider_id":"fixture","model":"m1","proxy":"disabled","models":[{"provider":"fixture","id":"m1","capabilities":{"tools":True,"reasoning_efforts":["none","low","high"]}}]}},"settings":{"provider_profile":"work","execution":{"stream":False}}}))
        master,slave=pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",30,140,0,0))
        child=subprocess.Popen([str(args.binary.resolve()),"--workspace",str(workspace),"--user-config",str(settings),"--no-resources","--reasoning-effort","high"],env={"PATH":os.environ.get("PATH",""),"TERM":"xterm-256color","AICODER_HOME":str(state),"FIXTURE_KEY":"fixture-key"},stdin=slave,stdout=slave,stderr=slave,close_fds=True)
        os.close(slave); output=bytearray()
        def drain():
            while select.select([master],[],[],0)[0]:
                try: chunk=os.read(master,65536)
                except OSError: return
                if not chunk:return
                output.extend(chunk)
                if len(output)>1024*1024: del output[:-1024*1024]
        def until(test,label):
            deadline=time.monotonic()+8
            while time.monotonic()<deadline:
                drain()
                if test(): return
                if child.poll() is not None: raise RuntimeError(f"TUI exited during {label}: {output[-2000:]!r}")
                time.sleep(.01)
            raise RuntimeError(f"{label} deadline: {output[-2000:]!r}")
        def send(text): os.write(master,text.encode())
        def command(text,marker):
            output.clear();send(text+"\r");until(lambda: marker.encode() in output, text)
        def turn_ends():
            count=0
            for file in (state/"sessions").glob("*.jsonl"):
                for raw in file.read_text().splitlines():
                    try: record=json.loads(raw)
                    except ValueError: continue
                    count+=sum(e["data"]["kind"]=="turn_end" for e in record.get("entries",[]))
            return count
        def turn(number,effort):
            output.clear();send(f"turn {number}\r")
            until(lambda: len(requests)==number and f"done-{number}".encode() in output,f"turn {number}")
            assert requests[-1].get("reasoning_effort")==effort,requests[-1]
            assert requests[-1]["max_completion_tokens"]==4096 and "max_tokens" not in requests[-1]
            # Wait for core completion / idle handling, not only the first content delta.
            until(lambda: b"completed" in output and turn_ends()>=number,f"terminal {number}")
            time.sleep(.05); drain()
        try:
            until(lambda:b"Sessions" in output,"startup")
            command("/thinking low","reasoning low")
            assert len(requests)==0 and not list((state/"sessions").glob("*.jsonl"))
            turn(1,"low")
            command("/thinking default","reasoning default");turn(2,None)
            send("\tn");time.sleep(.1);drain()
            command("/thinking default","reasoning default");turn(3,None);turn(4,None)
            command("/thinking max","does not support");assert len(requests)==4
            turn(5,None)
            send("\x03");assert child.wait(timeout=8)==0
            os.close(master)
            master,slave=pty.openpty(); fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack("HHHH",30,140,0,0))
            child=subprocess.Popen([str(args.binary.resolve()),"--workspace",str(workspace),"--user-config",str(settings),"--no-resources","--reasoning-effort","high"],env={"PATH":os.environ.get("PATH",""),"TERM":"xterm-256color","AICODER_HOME":str(state),"FIXTURE_KEY":"fixture-key"},stdin=slave,stdout=slave,stderr=slave,close_fds=True)
            os.close(slave); output.clear();until(lambda:b"Sessions" in output,"second startup")
            send("wait-provider\r");until(lambda:len(requests)==6,"pending request")
            os.close(master);master=None;child.wait(timeout=8)
            assert turn_ends()==6,"terminal IO failure abandoned the run journal"
            stops=[]
            for file in (state/"sessions").glob("*.jsonl"):
                for raw in file.read_text().splitlines():
                    record=json.loads(raw)
                    stops.extend(e["data"]["record"]["stop"] for e in record.get("entries",[]) if e["data"]["kind"]=="turn_end")
            assert any(stop and stop["kind"]=="cancelled" for stop in stops)
            print(json.dumps({"pty":True,"reasoning_requests":[r.get("reasoning_effort") for r in requests],"new_session_preview_read_only":True,"explicit_default_survives_next_turn":True,"unsupported_does_not_poison":True,"terminal_io_cleanup_awaited":True}))
        finally:
            if child.poll() is None: child.kill();child.wait()
            if master is not None: os.close(master)
finally:
    server.shutdown(); server.server_close()
