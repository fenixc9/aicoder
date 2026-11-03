#!/usr/bin/env python3
"""Native image reads in CLI/RPC/Web/real TUI: bounded fixtures, no remote model or credentials.
Build the three frontends with Web's server,images features before running.
"""
import argparse
import base64
import fcntl
import http.client
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
parser.add_argument('--bin-dir', type=Path, default=ROOT/'target/debug')
parser.add_argument("--code-mode", action="store_true", help="Exercise explicit and filtered Code Mode image output")
parser.add_argument("--only", action="store_true", help="Exercise Code Mode only root exposure and direct-call rejection")
args = parser.parse_args()
args.code_mode = args.code_mode or args.only
BINS = args.bin_dir.resolve()
TOKEN = 'fixture-file-images-token-0123456789'
requests = []
failures = []


def bmp():
    return (struct.pack('<2sIHHI', b'BM', 62, 0, 0, 54) +
            struct.pack('<IiiHHIIIIII', 40, 2, 1, 1, 24, 0, 8, 0, 0, 0, 0) +
            bytes([0, 0, 255, 0, 255, 0, 0, 0]))


class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        try:
            size = int(self.headers['Content-Length'])
            assert size < 2*1024*1024
            request = json.loads(self.rfile.read(size))
            requests.append(request)
            if args.only:
                assert [t['function']['name'] for t in request['tools']] == ['codemode']
            tools = [m for m in request['messages'] if m['role'] == 'tool']
            if not tools:
                code = "try {const full=await tools.read_file.result({path:'picture.txt'}); " + ("text('filtered');" if request['model']=='filtered' else "await image(full); text('selected');") + "} catch(e) {text(e.code); throw e;}"
                message = {'role': 'assistant', 'tool_calls': [{'id': 'native-image', 'type': 'function',
                    'function': {'name': 'codemode' if args.code_mode else 'read_file', 'arguments': json.dumps({'code':code} if args.code_mode else {'path':'picture.txt'})}}]}
                if request['model']=='bypass':
                    message['tool_calls'][0]['function']={'name':'write_file','arguments':json.dumps({'path':'forbidden.txt','content':'must not execute'})}
            else:
                assert len(tools) == 1
                result = json.loads(tools[0]['content'])
                if request['model']=='bypass':
                    assert result['is_error'] is True
                    assert result['content']['error']['code']=='tool_not_exposed'
                    assert result['content']['error']['details']['operation_completed'] is False
                elif request['model'] == 'nostore':
                    assert result['is_error'] is True
                    error=result['content']['error']
                    assert error['code'] == ('code_mode_script' if args.code_mode else 'artifact_store_unavailable')
                    if args.code_mode:
                        assert 'artifact_store_unavailable' in error['details']['items']
                else:
                    assert result['ok'] is True
                    if args.code_mode:
                        assert result['output']['program']['items'] == (['filtered'] if request['model']=='filtered' else ['selected'])
                        assert result['output']['composition']['calls'] == (1 if request['model']=='filtered' else 2)
                    else:
                        assert result['output']['kind'] == 'image'
                        assert result['output']['source_bytes'] == 62
                        assert result['output']['media_type'] == 'image/bmp'
                    if request['model']=='filtered':
                        assert not result.get('artifacts')
                        assert request['messages'][-1]['role']=='tool'
                        status='filtered'
                    else:
                        status=result['_tool_image_projection'][0]['status']
                    if request['model'] == 'filtered':
                        pass
                    elif request['model'] == 'text':
                        assert status == 'model_without_vision'
                        assert request['messages'][-1]['role'] == 'tool'
                    else:
                        assert status == 'normalized_preview'
                        tail = request['messages'][-1]
                        assert tail['role'] == 'user'
                        parts = [p for p in tail['content'] if p['type'] == 'image_url']
                        assert len(parts) == 1
                        url = parts[0]['image_url']['url']
                        assert url.startswith('data:image/jpeg;base64,')
                        jpeg = base64.b64decode(url.split(',', 1)[1], validate=True)
                        assert jpeg.startswith(b'\xff\xd8') and len(jpeg) <= 256*1024
                message = {'role': 'assistant', 'content': 'native image done'}
            body = json.dumps({'id': 'fixture', 'object': 'chat.completion', 'created': 1,
                'model': request['model'], 'choices': [{'index': 0, 'message': message,
                'finish_reason': 'stop' if tools else 'tool_calls'}],
                'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}}).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except Exception as error:
            failures.append(repr(error))
            raise

    def log_message(self, *_):
        pass


def until(test, label):
    deadline = time.monotonic()+12
    while time.monotonic() < deadline:
        result = test()
        if result:
            return result
        time.sleep(.01)
    raise RuntimeError(label+' deadline')


def line(pipe):
    data = bytearray()
    deadline = time.monotonic()+12
    while not data.endswith(b'\n'):
        remaining = deadline-time.monotonic()
        if remaining <= 0 or not select.select([pipe], [], [], remaining)[0]:
            raise RuntimeError('frame deadline')
        chunk = os.read(pipe.fileno(), 1)
        if not chunk or len(data) >= 8*1024*1024:
            raise RuntimeError('incomplete/oversized frame')
        data.extend(chunk)
    return data


def stop(child):
    if child.poll() is None:
        child.kill()
        child.wait(timeout=5)


def entries(root):
    records = [json.loads(raw) for file in (root/'state').rglob('*.jsonl')
               for raw in file.read_text().splitlines()]
    return [e['data'] for record in records for e in record.get('entries', [])]


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix='aicoder-native-images-') as folder:
        for frontend in ['cli', 'rpc', 'web', 'tui', 'text', 'nostore'] + (['filtered'] if args.code_mode else []) + (['bypass'] if args.only else []):
            root = Path(folder)/frontend
            root.mkdir()
            (root/'picture.txt').write_bytes(bmp())
            state = root/'state'
            state.mkdir(mode=0o700)
            model = frontend if frontend in ['text', 'nostore', 'filtered', 'bypass'] else 'vision'
            (state/'settings.json').write_text(json.dumps({'version': 1, 'profiles': {'fixture': {
                'base_url': f'http://127.0.0.1:{server.server_address[1]}/v1',
                'provider_id': 'fixture', 'model': model, 'api_key_env': 'FIXTURE_KEY', 'proxy': 'disabled',
                'models': [{'provider': 'fixture', 'id': m, 'context_tokens': 65536,
                            'capabilities': {'tools': True, 'images': m != 'text'}} for m in ['vision', 'text', 'nostore', 'filtered', 'bypass']]}},
                'settings': {'provider_profile': 'fixture', 'execution': {'stream': False}, 'code_mode':{'enabled':args.code_mode}}}))
            env = {'PATH': os.environ.get('PATH', ''), 'TERM': 'xterm-256color',
                   'AICODER_HOME': str(state), 'FIXTURE_KEY': 'fixture-key', 'AICODER_WEB_TOKEN': TOKEN}
            common = ['--workspace', str(root), '--no-resources']
            if args.only:
                common += ['--code-mode-only']
            count = len(requests)
            if frontend in ['cli', 'text', 'nostore', 'filtered', 'bypass']:
                output = subprocess.run([str(BINS/'aicoder'), *common,
                    *(['--no-session'] if frontend == 'nostore' else []), *(['--yes'] if args.code_mode else []), '--json', '--prompt', 'inspect native image'],
                    env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=20)
                assert output.returncode == 0, output.stderr
                events = [json.loads(raw)['event'] for raw in output.stdout.splitlines()]
                assert any(e['type'] == 'agent_completed' for e in events)
                assert not any(e['type'] == 'run_approval_requested' for e in events)
            elif frontend == 'rpc':
                child = subprocess.Popen([str(BINS/'aicoder'), *common, '--rpc'], env=env,
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
                try:
                    sequence=1
                    child.stdin.write(b'{"version":1,"id":1,"command":{"type":"prompt","text":"inspect native image"}}\n')
                    while True:
                        frame = json.loads(line(child.stdout))
                        if frame['type'] == 'event':
                            event=frame['event']['event']
                            if event['type']=='run_approval_requested':
                                assert args.code_mode
                                question=event['request']; assert question['invocation']['name']=='codemode'
                                sequence+=1
                                child.stdin.write((json.dumps({'version':1,'id':sequence,'command':{'type':'approve','run_id':question['id']['run_id'],'approval_id':question['id'],'decision':'allow_once'}})+'\n').encode())
                        if frame['type'] == 'run_finished':
                            assert frame['result']['status'] == 'completed', frame
                            break
                    sequence+=1
                    child.stdin.write((json.dumps({'version':1,'id':sequence,'command':{'type':'shutdown'}})+'\n').encode())
                    while json.loads(line(child.stdout)).get('id') != sequence:
                        pass
                    assert child.wait(timeout=8) == 0
                finally:
                    stop(child)
                    child.stdin.close()
                    child.stdout.close()
                    child.stderr.close()
            elif frontend == 'web':
                child = subprocess.Popen([str(BINS/'aicoder-web'), *common, '--listen', '127.0.0.1:0'],
                    env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, bufsize=0)
                try:
                    ready = line(child.stderr).decode().strip()
                    prefix = 'Web example listening at http://'
                    assert ready.startswith(prefix), ready
                    connection = http.client.HTTPConnection(ready.removeprefix(prefix), timeout=10)

                    def api(method, path, payload=None):
                        connection.request(method, path, None if payload is None else json.dumps(payload),
                            {'Authorization': 'Bearer '+TOKEN, 'Content-Type': 'application/json'})
                        response = connection.getresponse()
                        value = json.loads(response.read())
                        assert 200 <= response.status < 300, value
                        return value

                    run = api('POST', '/api/runs', {'id': 1, 'text': 'inspect native image'})
                    path = '/api/runs/'+run['run_id']
                    def poll_run():
                        state=api('GET',path)
                        for question in state['approvals']:
                            assert args.code_mode and question['invocation']['name']=='codemode'
                            api('POST',path+'/approval',{'approval_id':question['id'],'decision':'allow_once'})
                        return state if state['result'] else None
                    result = until(poll_run, 'Web completion')
                    assert not result['approvals'] and result['result']['status'] == 'completed', result
                    connection.close()
                    child.terminate()
                    assert child.wait(timeout=8) == 0
                finally:
                    stop(child)
                    child.stderr.close()
            else:
                master, slave = pty.openpty()
                fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 140, 0, 0))
                child = subprocess.Popen([str(BINS/'aicoder-tui'), *common], env=env,
                    stdin=slave, stdout=slave, stderr=slave, close_fds=True)
                os.close(slave)
                output = bytearray()

                def marker(text):
                    while select.select([master], [], [], 0)[0]:
                        try:
                            chunk = os.read(master, 65536)
                        except OSError:
                            break
                        if not chunk:
                            break
                        output.extend(chunk)
                        if len(output) > 1024*1024:
                            del output[:-1024*1024]
                    assert child.poll() is None, output[-2000:]
                    return text.encode() in output

                def completed():
                    marker('')
                    rows = entries(root)
                    return any(e.get('kind') == 'turn_end' for e in rows) and 'native image done' in json.dumps(rows)

                try:
                    until(lambda: marker('Sessions'), 'TUI startup')
                    os.write(master, b'inspect native image\r')
                    if args.code_mode:
                        until(lambda: marker('codemode') and marker('Approval'),'TUI program approval')
                        os.write(master,b'y')
                    until(completed, 'TUI canonical completion')
                    time.sleep(.15)
                    marker('')
                    os.write(master, b'\x03')
                    assert child.wait(timeout=8) == 0
                finally:
                    stop(child)
                    os.close(master)
            assert len(requests) == count+2, (frontend, len(requests)-count, failures)
            assert (root/'picture.txt').read_bytes() == bmp()
            assert not (root/'forbidden.txt').exists()
            if frontend != 'nostore':
                rows = entries(root)
                messages = [e['message'] for e in rows if e.get('kind') == 'message']
                images = [b for m in messages for b in m['content'] if b['type'] == 'image']
                if frontend=='bypass':
                    assert not images, frontend
                elif frontend=='filtered':
                    assert not images, frontend
                    assert any(e.get('namespace')=='aicoder.composition.v1' and '"type": "image"' in json.dumps(e.get('data')) for e in rows)
                else:
                    assert len(images) == 1 and images[0]['source']['type'] == 'artifact', frontend
                    assert images[0]['source']['reference']['media_type'] == 'image/bmp'
                assert sum(m['role'] == 'tool' for m in messages) == 1
                assert sum(m['role'] == 'user' for m in messages) == 1
                assert '_tool_image_projection' not in json.dumps(rows)
        assert not failures, failures
        print(json.dumps({'native_read_file': True, 'code_mode':args.code_mode, 'only_mode':args.only, 'direct_hidden_call_rejected':args.only, 'explicit_image_selection':args.code_mode, 'signature_not_extension': True, 'bmp': True,
            'frontends': ['cli_json', 'rpc', 'web', 'tui_pty'], 'nonvision_omission': True,
            'missing_storage_explicit': True, 'source_unchanged': True, 'raw_canonical_artifacts': True,
            'synthetic_user_history': False, 'loopback_model_requests': len(requests), 'real_model_requests': 0}))
finally:
    server.shutdown()
    server.server_close()
