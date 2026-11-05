#!/usr/bin/env python3
"""Real terminal raw-input UI before model startup and while a model request remains active."""
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
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/examples/offline_extension_ui')
args=parser.parse_args()
with tempfile.TemporaryDirectory(prefix='aicoder-input-hooks-') as directory:
    root=Path(directory)
    master,slave=pty.openpty()
    fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,140,0,0))
    child=subprocess.Popen([str(args.binary.resolve()),str(root/'sessions')],env={'PATH':os.environ.get('PATH',''),'TERM':'xterm-256color'},stdin=slave,stdout=slave,stderr=slave,close_fds=True)
    os.close(slave)
    output=bytearray()
    def drain():
        while select.select([master],[],[],0)[0]:
            try:chunk=os.read(master,65536)
            except OSError:return
            if not chunk:return
            output.extend(chunk)
            if len(output)>1024*1024:del output[:-1024*1024]
    def until(predicate,label):
        deadline=time.monotonic()+9
        while time.monotonic()<deadline:
            drain()
            if predicate():return
            if child.poll() is not None:raise RuntimeError(f'Host exited during {label}: {output[-1000:]!r}')
            time.sleep(.01)
        raise RuntimeError(f'{label} deadline: {output[-1500:]!r}')
    def marker(text):return text.encode() in output
    def send(text):os.write(master,text.encode())
    def entries():
        result=[]
        for path in (root/'sessions').glob('*.jsonl'):
            body=path.read_bytes()
            lines=body.splitlines()
            if not body.endswith(b'\n'):lines=lines[:-1]
            for line in lines:result.extend(e['data'] for e in json.loads(line).get('entries',[]))
        return result
    def messages(role):return [e['message'] for e in entries() if e['kind']=='message' and e['message']['role']==role]
    try:
        until(lambda:marker('Sessions'),'startup')
        send('/raw-ui\r')
        until(lambda:marker('256'),'initial raw input modal')
        send('\x1b[200~RAW_UI_PRIVATE\nline two\x1b[201~')
        time.sleep(.1);drain();assert not messages('user')
        output.clear();send('\r')
        until(lambda:marker('Input consumed'),'handled initial input')
        assert not messages('user') and not messages('assistant')
        assert 'RAW_UI_PRIVATE' not in json.dumps(entries())
        time.sleep(.1);output.clear();send('model-wait\r')
        until(lambda:len(messages('user'))==1,'owned model request')
        output.clear();send('/raw-ui\r')
        until(lambda:marker('256'),'raw steering modal')
        send('CONTROL_PRIVATE\r')
        until(lambda:marker('Input consumed'),'handled control acknowledgement')
        assert len(messages('user'))==1
        until(lambda:len(messages('assistant'))==1,'original model completion')
        assert len(messages('user'))==1
        encoded=json.dumps(entries())
        assert 'CONTROL_PRIVATE' not in encoded and '/raw-ui' not in encoded
        time.sleep(.2);send('\x03')
        try:child.wait(timeout=2)
        except subprocess.TimeoutExpired:send('\x03');child.wait(timeout=5)
        assert child.returncode==0
    finally:
        if child.poll() is None:child.kill();child.wait()
        os.close(master)
print(json.dumps({'pty':True,'initial_input_ui':True,'handled_without_fake_turn':True,'paste_not_submit':True,'control_input_ui':True,'original_model_finishes_once':True,'raw_bodies_not_persisted':True}))
