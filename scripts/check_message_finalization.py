#!/usr/bin/env python3
"""Real PTY finalization -> canonical history and effective visible assistant."""
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
with tempfile.TemporaryDirectory(prefix='aicoder-message-finalization-') as directory:
    root=Path(directory);master,slave=pty.openpty()
    fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,140,0,0))
    child=subprocess.Popen([str(args.binary.resolve()),str(root/'sessions')],env={'PATH':os.environ.get('PATH',''),'TERM':'xterm-256color'},stdin=slave,stdout=slave,stderr=slave,close_fds=True)
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
    def until(predicate,label):
        deadline=time.monotonic()+8
        while time.monotonic()<deadline:
            drain()
            if predicate():return
            if child.poll() is not None:raise RuntimeError(f'Host exited during {label}: {output[-1000:]!r}')
            time.sleep(.01)
        raise RuntimeError(f'{label}: {output[-1500:]!r}')
    try:
        until(lambda:b'Sessions' in output,'startup');os.write(master,b'message-finalization-demo\r')
        until(lambda:any(e['kind']=='turn_end' for e in entries()),'finalized turn')
        until(lambda:b'FINALIZED_ALL_ROLE_ANSWER' in output,'finalized visible answer')
        values=entries()
        assistants=[e['message'] for e in values if e['kind']=='message' and e['message']['role']=='assistant']
        assert len(assistants)==1 and assistants[0]['content']==[{'type':'text','text':'FINALIZED_ALL_ROLE_ANSWER'}]
        assert 'Reviewed input: EFFECTIVE_TUI_USER' not in json.dumps(values)
        users=[e['message'] for e in values if e['kind']=='message' and e['message']['role']=='user']
        assert len(users)==1 and users[0]['content']==[{'type':'text','text':'EFFECTIVE_TUI_USER'}]
        assert 'message-finalization-demo' not in json.dumps(values)
        receipts=[e['data'] for e in values if e['kind']=='extension' and e['namespace']=='aicoder.message-finalization.v1']
        assert sum(r['type']=='applied' for r in receipts)==2
        assert {r['receipt']['role'] for r in receipts if r['type']=='applied'}=={'user','assistant'}
        time.sleep(.2);os.write(master,b'\x03')
        try:child.wait(timeout=2)
        except subprocess.TimeoutExpired:os.write(master,b'\x03');child.wait(timeout=5)
        assert child.returncode==0
    finally:
        if child.poll() is None:child.kill();child.wait()
        os.close(master)
print(json.dumps({'pty':True,'finalized_visible_answer':True,'canonical_assistants':1,'canonical_users':1,'raw_preview_not_persisted':True,'safe_receipts':True,'network_requests':0}))
