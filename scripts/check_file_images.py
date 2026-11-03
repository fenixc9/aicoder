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
args = parser.parse_args()
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
            tools = [m for m in request['messages'] if m['role'] == 'tool']
            if not tools:
                message = {'role': 'assistant', 'tool_calls': [{'id': 'native-image', 'type': 'function',
                    'function': {'name': 'read_file', 'arguments': json.dumps({'path': 'picture.txt'})}}]}
            else:
                assert len(tools) == 1
                result = json.loads(tools[0]['content'])
                if request['model'] == 'nostore':
                    assert result['is_error'] is True
                    assert result['content']['error']['code'] == 'artifact_store_unavailable'
                else:
                    assert result['ok'] is True and result['output']['kind'] == 'image'
                    assert result['output']['source_bytes'] == 62
                    assert result['output']['media_type'] == 'image/bmp'
                    status = result['_tool_image_projection'][0]['status']
                    if request['model'] == 'text':
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
        for frontend in ['cli', 'rpc', 'web', 'tui', 'text', 'nostore']:
            root = Path(folder)/frontend
            root.mkdir()
            (root/'picture.txt').write_bytes(bmp())
            state = root/'state'
            state.mkdir(mode=0o700)
            model = frontend if frontend in ['text', 'nostore'] else 'vision'
            (state/'settings.json').write_text(json.dumps({'version': 1, 'profiles': {'fixture': {
                'base_url': f'http://127.0.0.1:{server.server_address[1]}/v1',
                'provider_id': 'fixture', 'model': model, 'api_key_env': 'FIXTURE_KEY', 'proxy': 'disabled',
                'models': [{'provider': 'fixture', 'id': m, 'context_tokens': 65536,
                            'capabilities': {'tools': True, 'images': m != 'text'}} for m in ['vision', 'text', 'nostore']]}},
                'settings': {'provider_profile': 'fixture', 'execution': {'stream': False}}}))
            env = {'PATH': os.environ.get('PATH', ''), 'TERM': 'xterm-256color',
                   'AICODER_HOME': str(state), 'FIXTURE_KEY': 'fixture-key', 'AICODER_WEB_TOKEN': TOKEN}
            common = ['--workspace', str(root), '--no-resources']
            count = len(requests)
            if frontend in ['cli', 'text', 'nostore']:
                output = subprocess.run([str(BINS/'aicoder'), *common,
                    *(['--no-session'] if frontend == 'nostore' else []), '--json', '--prompt', 'inspect native image'],
                    env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=20)
                assert output.returncode == 0, output.stderr
                events = [json.loads(raw)['event'] for raw in output.stdout.splitlines()]
                assert any(e['type'] == 'agent_completed' for e in events)
                assert not any(e['type'] == 'run_approval_requested' for e in events)
            elif frontend == 'rpc':
                child = subprocess.Popen([str(BINS/'aicoder'), *common, '--rpc'], env=env,
                    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
                try:
                    child.stdin.write(b'{"version":1,"id":1,"command":{"type":"prompt","text":"inspect native image"}}\n')
                    while True:
                        frame = json.loads(line(child.stdout))
                        if frame['type'] == 'event':
                            assert frame['event']['event']['type'] != 'run_approval_requested'
                        if frame['type'] == 'run_finished':
                            assert frame['result']['status'] == 'completed', frame
                            break
                    child.stdin.write(b'{"version":1,"id":2,"command":{"type":"shutdown"}}\n')
                    while json.loads(line(child.stdout)).get('id') != 2:
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
                    result = until(lambda: (s if (s := api('GET', path))['result'] else None), 'Web completion')
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
            if frontend != 'nostore':
                rows = entries(root)
                messages = [e['message'] for e in rows if e.get('kind') == 'message']
                images = [b for m in messages for b in m['content'] if b['type'] == 'image']
                assert len(images) == 1 and images[0]['source']['type'] == 'artifact', frontend
                assert images[0]['source']['reference']['media_type'] == 'image/bmp'
                assert sum(m['role'] == 'tool' for m in messages) == 1
                assert sum(m['role'] == 'user' for m in messages) == 1
                assert '_tool_image_projection' not in json.dumps(rows)
        assert not failures, failures
        print(json.dumps({'native_read_file': True, 'signature_not_extension': True, 'bmp': True,
            'frontends': ['cli_json', 'rpc', 'web', 'tui_pty'], 'nonvision_omission': True,
            'missing_storage_explicit': True, 'source_unchanged': True, 'raw_canonical_artifacts': True,
            'synthetic_user_history': False, 'loopback_model_requests': len(requests), 'real_model_requests': 0}))
finally:
    server.shutdown()
    server.server_close()
