#!/usr/bin/env python3
"""Offline real-PTY TUI interaction check; stdlib only, fake provider and credentials."""
import argparse
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import tempfile
import termios
import threading
import time
ROOT=Path(__file__).resolve().parents[1]
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=Path,default=ROOT/'target/debug/aicoder-tui')
args=parser.parse_args()
requests=[]
release=threading.Event()
class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        value=json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        requests.append(value)
        last=value['messages'][-1]
        text=json.dumps(last.get('content',''))
        if last['role']=='user' and text=='"hold"':
            if not release.wait(8): raise RuntimeError('held fixture deadline')
        message={'role':'assistant','content':f'done-{len(requests)}'}
        finish='stop'
        if last['role']=='user' and text=='"mutate"':
            message={'role':'assistant','content':None,'tool_calls':[{'id':'guarded-write','type':'function','function':{'name':'write_file','arguments':json.dumps({'path':'never.txt','content':'must not be approved by paste'})}}]}
            finish='tool_calls'
        body=json.dumps({'id':'fixture','object':'chat.completion','created':1,'model':value['model'],'choices':[{'index':0,'message':message,'finish_reason':finish}],'usage':{'prompt_tokens':1,'completion_tokens':1,'total_tokens':2}}).encode()
        self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(body)));self.end_headers();self.wfile.write(body)
    def log_message(self,*_): pass
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider)
threading.Thread(target=server.serve_forever,daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix='aicoder-tui-interaction-') as folder:
        root=Path(folder);workspace=root/'workspace';workspace.mkdir();state=root/'state';state.mkdir()
        referenced=workspace/'context.txt';referenced.write_text('original file page <&>\nsecond line')
        settings=root/'settings.json';theme=root/'theme.json';theme.write_text(json.dumps({'base':'dark','accent':'#123456'}))
        settings.write_text(json.dumps({'version':1,'profiles':{'work':{'base_url':f'http://127.0.0.1:{server.server_address[1]}/v1','api_key_env':'FIXTURE_KEY','provider_id':'fixture','model':'m1','proxy':'disabled','models':[{'provider':'fixture','id':model,'context_tokens':65536,'capabilities':{'tools':True,'reasoning_efforts':['none','low','high']},'pricing':{'currency':'USD','input_per_million':2,'output_per_million':4}} for model in ['m1','m2']]}},'settings':{'provider_profile':'work','execution':{'stream':False}}}))
        master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,140,0,0))
        child=subprocess.Popen([str(args.binary.resolve()),'--workspace',str(workspace),'--user-config',str(settings),'--no-resources','--theme-file',str(theme)],env={'PATH':os.environ.get('PATH',''),'TERM':'xterm-256color','AICODER_HOME':str(state),'FIXTURE_KEY':'fixture-key'},stdin=slave,stdout=slave,stderr=slave,close_fds=True)
        os.close(slave);output=bytearray()
        def drain():
            while select.select([master],[],[],0)[0]:
                try: chunk=os.read(master,65536)
                except OSError:return
                if not chunk:return
                output.extend(chunk)
                if len(output)>1024*1024:del output[:-1024*1024]
        def until(test,label):
            end=time.monotonic()+10
            while time.monotonic()<end:
                drain()
                if test():return
                if child.poll() is not None:raise RuntimeError(f'TUI exited during {label}: {output[-2000:]!r}')
                time.sleep(.01)
            raise RuntimeError(f'{label} deadline; requests={len(requests)}, turns={len(ends())}: {output[-1500:]!r}')
        def send(text):os.write(master,text.encode())
        def submit(text,expected):
            send(text+'\r')
            end=time.monotonic()+10
            # A closed control can reject before the owner result reaches the UI; the draft
            # remains. Empty Enter is inert after admission, so only that retained draft retries.
            while len(requests)<expected and time.monotonic()<end:
                drain();time.sleep(.1)
                if len(requests)<expected:send('\r')
            until(lambda:len(requests)>=expected,'submit '+text)
        def marker(text):return text.encode() in output
        def entries():
            values=[]
            for file in (state/'sessions').glob('*.jsonl'):
                for raw in file.read_text().splitlines():
                    try:record=json.loads(raw)
                    except ValueError:continue
                    values.extend(e['data'] for e in record.get('entries',[]))
            return values
        def command(text,expected,applied=None):
            output.clear();send(text+'\r')
            if applied:
                deadline=time.monotonic()+10
                while not applied() and time.monotonic()<deadline:
                    drain();time.sleep(.1)
                    if not applied():send('\r')
            until(lambda: applied() if applied else marker(expected),text)
        def ends():
            entries=[]
            for file in (state/'sessions').glob('*.jsonl'):
                for raw in file.read_text().splitlines():
                    try:record=json.loads(raw)
                    except ValueError:continue
                    entries.extend(e['data'] for e in record.get('entries',[]) if e['data']['kind']=='turn_end')
            return entries
        def finished(requests_count,turns):
            # Journal completion can precede delivery of the owner's result to the UI.
            # If the next Enter hit a closed handle, only its retained draft retries;
            # accepted/queued input has already been cleared, so empty Enter is inert.
            deadline=time.monotonic()+10
            while len(requests)<requests_count and time.monotonic()<deadline:
                drain();time.sleep(.1)
                if len(requests)<requests_count:send('\r')
            until(lambda:len(requests)==requests_count and len(ends())==turns,f'finish {requests_count}')
            time.sleep(.05);drain()
        try:
            until(lambda:marker('Sessions'),'startup')
            send('你好é👩‍💻');send('\x0f');send('line two');assert not requests;send('\r');finished(1,1)
            assert requests[0]['messages'][-1]['content']=='你好é👩‍💻\nline two'
            send('draft');send('\x10');send('\x0e');send('\r');finished(2,2);assert requests[-1]['messages'][-1]['content']=='draft'
            send('\x1b[200~pasted first\npasted second\x1b[201~');time.sleep(.1);drain();assert len(requests)==2
            send('\r');finished(3,3);assert requests[-1]['messages'][-1]['content']=='pasted first\npasted second'
            command('/file context.txt','Attached context.txt');referenced.write_text('changed on disk')
            send('inspect reference\r');finished(4,4);assert 'original file page' in requests[-1]['messages'][-1]['content'];assert 'changed on disk' not in requests[-1]['messages'][-1]['content']
            command('/usage','base_cost_estimate');assert marker('model_context');assert len(requests)==4
            send('/theme light\r')
            time.sleep(.1);fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',16,60,0,0));os.kill(child.pid,signal.SIGWINCH);time.sleep(.1);drain();assert child.poll() is None
            fcntl.ioctl(master,termios.TIOCSWINSZ,struct.pack('HHHH',30,140,0,0));os.kill(child.pid,signal.SIGWINCH);time.sleep(.1);drain()
            output.clear();send('hold\r');until(lambda:len(requests)==5,'held request')
            command('/file context.txt','Attached context.txt');output.clear();send('steered-reference\r');until(lambda:marker('Queued Steer'),'steer')
            # ANSI diff rendering can retain the unchanged 'Queued ' prefix from Steer.
            # The changed kind is visible; exact delivery is asserted against requests below.
            output.clear();send('after');send('\x1b\r');until(lambda:marker('FollowUp'),'follow-up');release.set();finished(7,5)
            assert 'steered-reference' in requests[5]['messages'][-1]['content'] and 'changed on disk' in requests[5]['messages'][-1]['content'];assert requests[6]['messages'][-1]['content']=='after'
            output.clear();submit('mutate',8);until(lambda:len(requests)==8 and marker('Approval'),'prepared approval');assert not (workspace/'never.txt').exists()
            send('\x1b[200~y\n\x1b[201~');time.sleep(.15);drain();assert len(requests)==8 and not (workspace/'never.txt').exists()
            send('n');finished(9,6);assert not (workspace/'never.txt').exists();assert 'approval' in json.dumps(requests[-1]['messages'][-1]).lower()
            command('/name tui-case','Session updated',lambda:any(e.get('namespace')=='aicoder.session.navigation.v1' and e.get('data',{}).get('title')=='tui-case' for e in entries()));command('/branch root','Session updated',lambda:any(e.get('namespace')=='aicoder.session.navigation.v1' and e.get('data',{}).get('operation')=='branch' for e in entries()));submit('branched',10);finished(10,7)
            command('/model m2','Model selected',lambda:any(e.get('kind')=='model_changed' and e.get('model')=='m2' for e in entries()));command('/thinking low','reasoning low',lambda:any(e.get('namespace')=='aicoder.reasoning.v1' and e.get('data',{}).get('model')=='m2' and e.get('data',{}).get('effort')=='low' for e in entries()));submit('model shifted',11);finished(11,8)
            assert requests[-1]['model']=='m2' and requests[-1]['reasoning_effort']=='low'
            assert all(r.get('max_completion_tokens')==4096 for r in requests)
            send('\x03');assert child.wait(timeout=8)==0
            print(json.dumps({'pty':True,'model_requests':len(requests),'turns':len(ends()),'multiline_unicode':True,'history_restored_draft':True,'paste_not_submit_or_approve':True,'immutable_reference':True,'reference_while_running':True,'steer_follow_up':True,'branch_model_reasoning':True,'context_cost_details':True,'theme_resize':True}))
        finally:
            release.set()
            if child.poll() is None:child.kill();child.wait()
            os.close(master)
finally:
    server.shutdown();server.server_close()
