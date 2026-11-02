#!/usr/bin/env python3
"""Offline real-PTY package palette selection; no model requests or real credentials."""
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
parser.add_argument('--bin-dir', type=Path, default=ROOT / 'target/debug')
options = parser.parse_args()
bins = options.bin_dir.resolve()
requests = []


class RejectModel(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        requests.append(self.path)
        self.send_error(500, 'Theme commands must not call a model')

    def do_GET(self):
        self.do_POST()

    def log_message(self, *_):
        pass


server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), RejectModel)
threading.Thread(target=server.serve_forever, daemon=True).start()
try:
    with tempfile.TemporaryDirectory(prefix='aicoder-package-themes-') as folder:
        root = Path(folder)
        workspace = root / 'workspace'
        workspace.mkdir()
        source = root / 'package'
        source.mkdir()
        marker = workspace / 'must-not-execute'
        (source / 'blue.json').write_text(json.dumps({'base': 'dark', 'accent': '#123456'}))
        (source / 'warm.json').write_text(json.dumps({'base': 'dark', 'accent': '#654321'}))
        (source / 'program').write_text("#!/bin/sh\ntouch '" + str(marker) + "'\n")
        (source / 'program').chmod(0o700)
        (source / 'aicoder-package.json').write_text(json.dumps({'schema_version': 1,
            'id': 'palette', 'version': '1', 'files': ['blue.json', 'warm.json', 'program'],
            'resources': {'themes': ['blue.json', 'warm.json']}, 'permissions': {'execution': True},
            'executables': [{'id': 'never', 'protocol': 'process', 'program': {'kind': 'bundled', 'path': 'program'}}]}))
        state = root / 'state'
        env = {'PATH': os.environ.get('PATH', ''), 'TERM': 'xterm-256color',
            'AICODER_HOME': str(state), 'OPENAI_API_KEY': 'fixture-key', 'AICODER_MODEL': 'test',
            'AICODER_API_BASE': f'http://127.0.0.1:{server.server_address[1]}/v1',
            'AICODER_STREAM': 'false', 'AICODER_PROXY': 'disabled', 'AICODER_MAX_RETRIES': '0'}
        installed = subprocess.check_output([str(bins / 'aicoder'), '--no-config', '--no-resources',
            '--workspace', str(workspace), 'packages', 'install', str(source), '--allow-execution'],
            env=env, timeout=10)
        record = json.loads(installed)
        assert not marker.exists() and not requests
        # Explicit UI selection works when the package is not a globally enabled root.
        subprocess.run([str(bins / 'aicoder'), '--no-config', '--workspace', str(workspace),
            'packages', 'disable', 'palette'], env=env, stdout=subprocess.DEVNULL, check=True, timeout=10)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 140, 0, 0))
        child = subprocess.Popen([str(bins / 'aicoder-tui'), '--no-config', '--no-resources',
            '--workspace', str(workspace), '--theme-package', 'palette:blue.json'], env=env,
            stdin=slave, stdout=slave, stderr=slave, close_fds=True)
        os.close(slave)
        output = bytearray()

        def drain():
            while select.select([master], [], [], 0)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    return
                if not data:
                    return
                output.extend(data)
                if len(output) > 1024 * 1024:
                    del output[:-1024 * 1024]

        def until(test, label):
            end = time.monotonic() + 10
            while time.monotonic() < end:
                drain()
                if test():
                    return
                if child.poll() is not None:
                    raise RuntimeError(f'TUI exited during {label}: {output[-1600:]!r}')
                time.sleep(.01)
            raise RuntimeError(f'{label} deadline: {output[-1600:]!r}')

        try:
            until(lambda: b'38;2;18;52;86' in output and b'Sessions' in output, 'packaged startup palette')
            output.clear()
            os.write(master, b'/theme palette:warm.json\r')
            until(lambda: b'38;2;101;67;33' in output, 'packaged runtime palette')
            output.clear()
            os.write(master, b'/theme palette:program\r')
            until(lambda: b'declared package theme' in output, 'reject undeclared theme kind')
            # A new selection must revalidate immutable content, not trust a prior UI capture.
            installed_root = state / 'packages/objects' / record['pin']['sha256']
            (installed_root / 'warm.json').write_text('{"accent":"#000000"}')
            output.clear()
            os.write(master, b'/theme palette:warm.json\r')
            until(lambda: b'changed' in output or b'integrity' in output or b'fingerprint' in output,
                  'reject changed installed content')
            assert not marker.exists() and not requests
            os.write(master, b'\x03')
            assert child.wait(timeout=8) == 0
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
            os.close(master)
        print(json.dumps({'pty': True, 'packaged_startup': True, 'packaged_slash_selection': True,
            'explicit_disabled_root_selection': True, 'declared_kind_enforced': True,
            'changed_content_rejected': True, 'package_code_not_executed': True, 'model_requests': len(requests)}))
finally:
    server.shutdown()
    server.server_close()
