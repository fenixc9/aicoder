#!/usr/bin/env python3
"""Offline CLI and real-PTY image checks: stdlib only, loopback and fake credentials."""
import argparse
import base64
import fcntl
import http.server
import json
import os
from pathlib import Path
import pty
import random
import select
import struct
import subprocess
import tempfile
import termios
import threading
import time
import zlib

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--bin-dir', type=Path, default=ROOT/'target/debug')
args = parser.parse_args()
requests = []
release = threading.Event()


def png():
    def chunk(kind, payload):
        return struct.pack('>I', len(payload))+kind+payload+struct.pack('>I', zlib.crc32(kind+payload))
    rng = random.Random(17)
    rows = b''.join(b'\0'+rng.randbytes(400*3) for _ in range(300))
    return (b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR', struct.pack('>IIBBBBB', 400, 300, 8, 2, 0, 0, 0))+
            chunk(b'IDAT', zlib.compress(rows))+chunk(b'IEND', b''))


def image_url(request):
    return next(p['image_url']['url'] for p in request['messages'][-1]['content'] if p['type'] == 'image_url')


class Provider(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        requests.append(request)
        if request['messages'][-1]['content'] == 'hold' and not release.wait(10):
            raise RuntimeError('held image fixture deadline')
        body = json.dumps({'id': 'fixture', 'object': 'chat.completion', 'created': 1,
                           'model': request['model'], 'choices': [{'index': 0, 'message': {
                               'role': 'assistant', 'content': 'image answer'}, 'finish_reason': 'stop'}],
                           'usage': {'prompt_tokens': 1, 'completion_tokens': 1, 'total_tokens': 2}}).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix='aicoder-images-') as folder:
        root = Path(folder)
        workspace = root/'workspace'
        workspace.mkdir()
        state = root/'state'
        state.mkdir()
        image = workspace/'image.bin'
        image.write_bytes(png())
        settings = root/'settings.json'
        settings.write_text(json.dumps({'version': 1, 'profiles': {'work': {
            'base_url': f'http://127.0.0.1:{server.server_address[1]}/v1', 'api_key_env': 'FIXTURE_KEY',
            'provider_id': 'fixture', 'model': 'vision', 'proxy': 'disabled',
            'models': [{'provider': 'fixture', 'id': model, 'context_tokens': 65536,
                        'capabilities': {'tools': True, 'images': model == 'vision'}} for model in ['vision', 'text']]}},
            'settings': {'provider_profile': 'work', 'execution': {'stream': False}}}))
        env = {'PATH': os.environ.get('PATH', ''), 'TERM': 'xterm-256color',
               'AICODER_HOME': str(state), 'FIXTURE_KEY': 'fixture-key'}
        common = ['--workspace', str(workspace), '--user-config', str(settings), '--no-resources']

        def cli(*flags):
            return subprocess.run([str(args.bin_dir.resolve()/'aicoder'), *common, *flags],
                                  env=env, stdin=subprocess.DEVNULL, capture_output=True, timeout=15)

        output = cli('--no-session', '--image', 'image.bin', '--print')
        assert output.returncode == 0, output.stderr
        assert output.stdout == b'image answer\n' and b'normalized' in output.stderr
        captured_url = image_url(requests[-1])
        assert captured_url.startswith('data:image/jpeg;base64,') and len(captured_url) > 65536
        assert base64.b64decode(captured_url.split(',', 1)[1]).startswith(b'\xff\xd8')
        output = cli('--no-session', '--image', 'image.bin', '--json', 'inspect')
        assert output.returncode == 0, output.stderr
        assert any(json.loads(line)['event']['type'] == 'agent_completed' for line in output.stdout.splitlines())
        count = len(requests)
        output = cli('--image', 'image.bin', '--model', 'text', '--print')
        assert output.returncode != 0 and not output.stdout and len(requests) == count
        assert b'image' in output.stderr.lower()
        output = cli('--image', 'image.bin', 'sessions')
        assert output.returncode != 0 and len(requests) == count
        image.write_bytes(b'not an image')
        assert cli('--no-session', '--image', 'image.bin', '--print').returncode != 0
        image.write_bytes(png())

        # A distinct TUI state makes all journal assertions independent from CLI attempts.
        tui_state = root/'tui-state'
        tui_state.mkdir()
        tui_env = dict(env, AICODER_HOME=str(tui_state))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 140, 0, 0))
        child = subprocess.Popen([str(args.bin_dir.resolve()/'aicoder-tui'), *common], env=tui_env,
                                 stdin=slave, stdout=slave, stderr=slave, close_fds=True)
        os.close(slave)
        screen = bytearray()

        def drain():
            while select.select([master], [], [], 0)[0]:
                try:
                    part = os.read(master, 65536)
                except OSError:
                    return
                if not part:
                    return
                screen.extend(part)
                if len(screen) > 1024*1024:
                    del screen[:-1024*1024]

        def until(test, label):
            deadline = time.monotonic()+12
            while time.monotonic() < deadline:
                drain()
                if test():
                    return
                if child.poll() is not None:
                    raise RuntimeError(f'TUI exited during {label}: {screen[-2000:]!r}')
                time.sleep(.01)
            raise RuntimeError(f'{label} deadline: {screen[-2500:]!r}')

        def send(text):
            os.write(master, text.encode())

        def send_turn(text, expected):
            send(text+'\r')
            deadline = time.monotonic()+10
            while len(requests) < expected and time.monotonic() < deadline:
                drain()
                time.sleep(.1)
                if len(requests) < expected:
                    send('\r')  # Retry only a draft rejected by a just-closed owner.
            until(lambda: len(requests) >= expected, 'turn admission')

        def entries():
            result = []
            for file in (tui_state/'sessions').glob('*.jsonl'):
                for line in file.read_text().splitlines():
                    try:
                        value = json.loads(line)
                    except ValueError:
                        continue
                    result.extend(e['data'] for e in value.get('entries', []))
            return result

        def turn_ends():
            return sum(e.get('kind') == 'turn_end' for e in entries())

        def command(text, marker, applied=None):
            screen.clear()
            send(text+'\r')
            if applied:
                deadline = time.monotonic()+10
                while not applied() and time.monotonic() < deadline:
                    drain()
                    time.sleep(.1)
                    if not applied():
                        send('\r')
            until(lambda: applied() if applied else marker.encode() in screen, text)

        try:
            until(lambda: b'Sessions' in screen, 'startup')
            command('/image image.bin', 'Attached image')
            image.write_bytes(b'replaced after capture')
            send_turn('', count+1)
            until(lambda: len(requests) == count+1 and turn_ends() == 1, 'image-only turn')
            assert image_url(requests[-1]) == captured_url
            assert base64.b64encode(b'\xff\xd8') not in screen
            image.write_bytes(png())
            command('/image image.bin', 'Attached image')
            send('/unimage all\r')
            time.sleep(.1)
            send('\r')
            time.sleep(.1)
            assert len(requests) == count+1
            screen.clear()
            send_turn('hold', count+2)
            until(lambda: len(requests) == count+2, 'held generation')
            command('/image image.bin', 'Attached image')
            command('steered image', 'Queued Steer')
            command('/image image.bin', 'Attached image')
            screen.clear()
            send('follow image\x1b\r')
            until(lambda: b'FollowUp' in screen, 'image follow-up')
            release.set()
            until(lambda: len(requests) == count+4 and turn_ends() == 2, 'image controls')
            assert image_url(requests[-2]) == captured_url and image_url(requests[-1]) == captured_url
            applied = [e for e in entries() if e.get('namespace') == 'aicoder.run.input']
            assert len(applied) == 2
            # Branch away from image history before selecting a text-only model.
            time.sleep(.1)
            drain()
            command('/branch root', 'Session updated', lambda: any(e.get('namespace') == 'aicoder.session.navigation.v1' and e.get('data', {}).get('operation') == 'branch' for e in entries()))
            command('/model text', 'Model selected', lambda: any(e.get('kind') == 'model_changed' and e.get('model') == 'text' for e in entries()))
            time.sleep(.1)
            drain()
            command('/image image.bin', 'Attached image')
            command('retained draft', 'does not declare image')
            assert len(requests) == count+4
            send('/unimage all\r')
            time.sleep(.1)
            send('\x03')
            assert child.wait(timeout=8) == 0
            print(json.dumps({'cli_print_json_image_only': True, 'unsupported_model_no_request': True,
                              'pty': True, 'immutable_capture': True, 'image_steer_follow_up': True,
                              'tui_capability_rejection': True, 'model_requests': len(requests)}))
        finally:
            release.set()
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)
finally:
    server.shutdown()
    server.server_close()
