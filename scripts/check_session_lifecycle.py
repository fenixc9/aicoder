#!/usr/bin/env python3
"""Real terminal session guard: no writes before a private choice, paste stays data."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import pty
import re
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
with tempfile.TemporaryDirectory(prefix='aicoder-session-lifecycle-') as directory:
    root=Path(directory);master,slave=pty.openpty()
    fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',32,160,0,0))
    child=subprocess.Popen([str(args.binary.resolve()),str(root/'sessions'),'--session-hooks'],env={'PATH':os.environ.get('PATH',''),'TERM':'xterm-256color'},stdin=slave,stdout=slave,stderr=slave,close_fds=True)
    os.close(slave);output=bytearray()
    def drain():
        while select.select([master],[],[],0)[0]:
            try:chunk=os.read(master,65536)
            except OSError:return
            if not chunk:return
            output.extend(chunk)
            if len(output)>1024*1024:del output[:-1024*1024]
    def entries():
        result=[]
        for file in (root/'sessions').glob('*.jsonl'):
            body=file.read_bytes();lines=body.splitlines()
            if not body.endswith(b'\n'):lines=lines[:-1]
            for line in lines:result.extend(e['data'] for e in json.loads(line).get('entries',[]))
        return result
    def snapshot():return {p.name:p.read_bytes() for p in (root/'sessions').glob('*.jsonl')}
    def until(predicate,label):
        deadline=time.monotonic()+10
        while time.monotonic()<deadline:
            drain()
            if predicate():return
            if child.poll() is not None:raise RuntimeError(f'Host exited during {label}: {output[-1200:]!r}')
            time.sleep(.01)
        plain=re.sub(rb'\x1b\[[0-9;?]*[A-Za-z]',b'',bytes(output))
        raise RuntimeError(f'{label}: {plain[-3000:]!r}')
    try:
        until(lambda:bool(snapshot()) and b'selected.' in output,'startup adoption')
        assert not any(e['kind']=='turn_start' for e in entries())
        os.write(master,b'hello\r');until(lambda:any(e['kind']=='turn_end' for e in entries()),'ordinary turn')
        assert any(e['kind']=='message' and e['message']['role']=='assistant' and 'Reviewed input: hello' in json.dumps(e['message']) for e in entries())
        time.sleep(.2);drain()
        before=snapshot();os.write(master,b'/name ask-rename\r')
        until(lambda:b'Allow' in output and b'Deny' in output,'native guard question')
        assert snapshot()==before
        os.write(master,b'\x1b[200~2\x1b[201~');time.sleep(.2);drain();assert snapshot()==before
        os.write(master,b'\x1b[B\r')
        until(lambda:b'operator' in output,'guard denial')
        assert snapshot()==before
        os.write(master,b'/name accepted\r')
        until(lambda:any(e['kind']=='extension' and e.get('namespace')=='aicoder.session.navigation.v1' and e['data'].get('operation')=='rename' and e['data'].get('title')=='accepted' for e in entries()),'owned commit')
        until(lambda:b'updated;' in output,'adoption display')
        after=snapshot();assert after.keys()==before.keys() and all(after[k].startswith(v) for k,v in before.items())
        values=entries();assert sum(e['kind']=='message' and e['message']['role']=='user' for e in values)==1
        assert sum(e['kind']=='message' and e['message']['role']=='assistant' for e in values)==1
        os.write(master,b'\x03');child.wait(timeout=5);assert child.returncode==0
    finally:
        if child.poll() is None:child.kill();child.wait()
        os.close(master)
print(json.dumps({'pty':True,'startup_without_turn':True,'guard_before_file_write':True,'paste_not_authorization':True,'denial_keeps_bytes':True,'owned_rename_committed':True,'canonical_users':1,'canonical_assistants':1,'network_requests':0}))
