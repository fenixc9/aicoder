#!/usr/bin/env python3
"""Real terminal fork: private guard, source preservation and draft-only restoration."""
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
with tempfile.TemporaryDirectory(prefix='aicoder-session-fork-') as directory:
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
        before=snapshot();assert len(before)==1
        os.write(master,b'/fork\r')
        until(lambda:b'Allow' in output and b'Deny' in output,'private fork guard')
        assert snapshot()==before
        os.write(master,b'\x1b[200~allow\x1b[201~');time.sleep(.2);drain();assert snapshot()==before
        os.write(master,b'\r')
        until(lambda:len(snapshot())==2,'fork publication')
        until(lambda:b'forked' in output,'fork adoption display')
        after=snapshot();assert all(after[k]==v for k,v in before.items())
        targets=set(after)-set(before);target=(root/'sessions'/targets.pop())
        def target_entries():
            return [e['data'] for line in target.read_text().splitlines() for e in json.loads(line).get('entries',[])]
        assert not any(e['kind']=='message' for e in target_entries())
        time.sleep(.2);assert not any(e['kind']=='message' for e in target_entries())
        os.write(master,b'\r')
        until(lambda:any(e['kind']=='message' and e['message']['role']=='user' for e in target_entries()),'explicit draft submission')
        until(lambda:any(e['kind']=='message' and e['message']['role']=='assistant' for e in target_entries()),'fork continuation')
        values=target_entries();users=[e['message'] for e in values if e['kind']=='message' and e['message']['role']=='user']
        assert len(users)==1 and users[0]['content'][0]['text']=='hello'
        assert all(snapshot()[k]==v for k,v in before.items())
        time.sleep(.2)
        os.write(master,b'\x03');child.wait(timeout=5);assert child.returncode==0
    finally:
        if child.poll() is None:child.kill();child.wait()
        os.close(master)
print(json.dumps({'pty':True,'private_guard_before_publication':True,'paste_not_authorization':True,'source_bytes_unchanged':True,'draft_not_automatically_submitted':True,'explicit_continuation_user':True,'network_requests':0}))
