#!/usr/bin/env python3
"""Real PTY check of native extension UI; stdlib only, temporary scripted SDK host."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import struct
import subprocess
import tempfile
import termios
import time
ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=ROOT/'target/debug/examples/offline_extension_ui')
args = parser.parse_args()
with tempfile.TemporaryDirectory(prefix='aicoder-extension-ui-') as folder:
    root = Path(folder)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 140, 0, 0))
    child = subprocess.Popen([str(args.binary.resolve()), str(root/'sessions')],
                             env={'PATH':os.environ.get('PATH',''), 'TERM':'xterm-256color'},
                             stdin=slave, stdout=slave, stderr=slave, close_fds=True)
    os.close(slave)
    output = bytearray()
    def drain():
        while select.select([master],[],[],0)[0]:
            try: chunk = os.read(master,65536)
            except OSError: return
            if not chunk: return
            output.extend(chunk)
            if len(output)>1024*1024: del output[:-1024*1024]
    def until(test,label):
        end=time.monotonic()+8
        while time.monotonic()<end:
            drain()
            if test(): return
            if child.poll() is not None: raise RuntimeError(f'Host exited during {label}: {output[-1500:]!r}')
            time.sleep(.01)
        raise RuntimeError(f'{label} deadline: {output[-2000:]!r}')
    def send(text): os.write(master,text.encode())
    def marker(text): return text.encode() in output
    def entries():
        values=[]
        for file in (root/'sessions').glob('*.jsonl'):
            for raw in file.read_text().splitlines():
                record=json.loads(raw)
                values.extend(entry['data'] for entry in record.get('entries',[]))
        return values
    def users(): return [v['message'] for v in entries() if v['kind']=='message' and v['message']['role']=='user']
    def command():
        output.clear();send('/ext review\r');until(lambda:marker('Keep a note'),'selection modal')
    try:
        until(lambda:marker('Sessions'),'startup')
        command();send('\x1b[200~y\n\x1b[201~');time.sleep(.1);drain();assert not users()
        output.clear();send('\r');until(lambda:marker('1024'),'input modal')
        send('\x1b[200~你好\nline two\x1b[201~');time.sleep(.1);drain();assert not users()
        send('\r');until(lambda:marker('Review noted'),'notice completion');assert not users()
        time.sleep(.1);drain();command();send('\x1b[B\r');until(lambda:marker('1024'),'second input modal')
        send('to agent\r');until(lambda:len(users())==1 and any(v['kind']=='turn_end' for v in entries()),'normal AgentInput submission')
        assert 'Review request: to agent' in json.dumps(users(),ensure_ascii=False)
        assert 'Review action' not in json.dumps(entries(),ensure_ascii=False)
        time.sleep(.1);drain();command();send('\x03');time.sleep(.2);drain();assert child.poll() is None;assert len(users())==1
        send('\r');time.sleep(.1);drain();assert len(users())==1
        send('\x03');child.wait(timeout=5);assert child.returncode==0
    finally:
        if child.poll() is None: child.kill();child.wait()
        os.close(master)
print(json.dumps({'pty':True,'selection':True,'bounded_unicode_input':True,'paste_not_submit':True,'notice_without_model':True,'command_submit_via_core':True,'cancel':True}))
