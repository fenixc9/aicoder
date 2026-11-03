#!/usr/bin/env python3
"""Offline CLI npm/Git acquisition over a local registry and actual Git HTTP backend."""
import argparse
import base64
import gzip
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import tarfile
import tempfile
import threading
import time
import urllib.parse

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/aicoder')
parser.add_argument('--disabled-binary', type=Path)
options = parser.parse_args()
binary = options.binary.resolve()
TOKEN = 'fixture-package-secret'
requests = []


def git(root, *arguments):
    return subprocess.check_output(['git', '-c', 'core.hooksPath=/dev/null', '-c', 'commit.gpgsign=false',
        '-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid', *arguments], cwd=root,
        env={'PATH': os.environ.get('PATH', ''), 'GIT_CONFIG_GLOBAL': '/dev/null', 'GIT_CONFIG_NOSYSTEM': '1'}, stderr=subprocess.DEVNULL)


with tempfile.TemporaryDirectory(prefix='aicoder-package-downloads-') as folder:
    root = Path(folder)
    workspace = root / 'workspace'
    workspace.mkdir()
    package = root / 'source'
    package.mkdir()
    marker = workspace / 'must-not-execute'
    manifest = {'schema_version': 1, 'id': 'remote', 'version': '1.0.0', 'files': ['rules.md', 'runner'],
        'resources': {'instructions': ['rules.md']}, 'permissions': {'instructions': True, 'execution': True},
        'executables': [{'id': 'never', 'protocol': 'process', 'program': {'kind': 'bundled', 'path': 'runner'}}]}
    (package / 'aicoder-package.json').write_text(json.dumps(manifest))
    (package / 'package.json').write_text(json.dumps({'name': '@scope/example', 'version': '1.0.0',
        'scripts': {'postinstall': "touch '" + str(marker) + "'"}}))
    (package / 'rules.md').write_text('REMOTE_RULES')
    (package / 'runner').write_text("#!/bin/sh\ntouch '" + str(marker) + "'\n")
    (package / 'runner').chmod(0o700)
    data = io.BytesIO()
    with tarfile.open(fileobj=data, mode='w', format=tarfile.USTAR_FORMAT) as archive:
        for file in package.iterdir():
            archive.add(file, arcname='package/' + file.name)
    compressed = gzip.compress(data.getvalue())
    digest = 'sha512-' + base64.b64encode(hashlib.sha512(compressed).digest()).decode()
    git(package, 'init', '-q')
    git(package, 'add', '.')
    git(package, 'commit', '-qm', 'fixture')
    commit = git(package, 'rev-parse', 'HEAD').decode().strip()
    repositories = root / 'repositories'
    repositories.mkdir()
    git(root, 'clone', '--bare', str(package), str(repositories / 'repo.git'))
    backend = Path(git(package, '--exec-path').decode().strip()) / 'git-http-backend'

    class Service(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            self.serve()

        def do_GET(self):
            self.serve()

        def serve(self):
            requests.append(self.path)
            if self.headers.get('Authorization') != 'Bearer ' + TOKEN:
                self.send_error(403)
                return
            if self.path.startswith('/git/'):
                parsed = urllib.parse.urlsplit(self.path)
                body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
                output = subprocess.check_output([str(backend)], input=body,
                    env={'PATH': os.environ.get('PATH', ''), 'GIT_PROJECT_ROOT': str(repositories),
                        'GIT_HTTP_EXPORT_ALL': '1', 'REQUEST_METHOD': self.command,
                        'PATH_INFO': parsed.path.removeprefix('/git'), 'QUERY_STRING': parsed.query,
                        'CONTENT_TYPE': self.headers.get('Content-Type', ''), 'CONTENT_LENGTH': str(len(body))})
                head, _, data = output.partition(b'\r\n\r\n')
                self.send_response(200)
                for row in head.decode().split('\r\n'):
                    if ':' in row:
                        name, value = row.split(':', 1)
                        self.send_header(name, value.strip())
            else:
                if self.path.endswith('/slow'):
                    time.sleep(2)
                data = compressed if self.path == '/archive' else json.dumps({'name': '@scope/example',
                    'version': '1.0.0', 'dist': {'tarball': f'http://127.0.0.1:{self.server.server_port}/archive',
                    'integrity': 'sha512-AQID' if self.path.endswith('/bad') else digest}}).encode()
                self.send_response(200)
            self.send_header('Content-Length', str(len(data)))
            self.end_headers()
            try:
                self.wfile.write(data)
            except (BrokenPipeError, ConnectionResetError):
                pass

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Service)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        url = f'http://127.0.0.1:{server.server_port}'
        state = root / 'state'
        env = {'PATH': os.environ.get('PATH', ''), 'AICODER_HOME': str(state), 'PKG_TOKEN': TOKEN}
        prefix = [str(binary), '--no-config', '--no-resources', '--workspace', str(workspace), 'packages']
        common = ['--allow-loopback-http', '--token-env', 'PKG_TOKEN', '--allow-execution']
        if options.disabled_binary:
            disabled_state = root / 'disabled-state'
            denied = subprocess.run([str(options.disabled_binary.resolve()), '--no-config', '--workspace', str(workspace),
                'packages', 'install-npm', '@scope/example'], env={**env, 'AICODER_HOME': str(disabled_state)},
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
            assert denied.returncode != 0 and b'package-downloads' in denied.stderr
            assert not disabled_state.exists() and not requests
        npm = json.loads(subprocess.check_output([*prefix, 'install-npm', '@scope/example', '--registry', url,
            *common], env=env, timeout=15))
        assert npm['source']['kind'] == 'npm' and npm['source']['version'] == '1.0.0'
        git_result = json.loads(subprocess.check_output([*prefix, 'install-git', url + '/git/repo.git',
            '--revision', commit, *common], env=env, timeout=15))
        assert git_result['source']['commit'] == commit and git_result['pin'] == npm['pin']
        assert not marker.exists()
        registry = state / 'packages/registry.json'
        before = registry.read_bytes()
        failed = subprocess.run([*prefix, 'install-npm', '@scope/example', '--registry', url, '--version', 'bad',
            *common], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=15)
        assert failed.returncode != 0 and not failed.stdout and TOKEN.encode() not in failed.stderr
        assert registry.read_bytes() == before and TOKEN.encode() not in before
        child = subprocess.Popen([*prefix, 'install-npm', '@scope/example', '--registry', url, '--version', 'slow',
            *common], env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 10
            while not any(path.endswith('/slow') for path in requests) and time.monotonic() < deadline:
                time.sleep(.01)
            assert any(path.endswith('/slow') for path in requests)
            child.send_signal(signal.SIGINT)
            stdout, stderr = child.communicate(timeout=8)
            assert child.returncode != 0 and not stdout and TOKEN.encode() not in stderr
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()
        assert registry.read_bytes() == before and not marker.exists()
        assert not list((state / 'packages/staging').iterdir())
        print(json.dumps({'npm': True, 'actual_git_http': True, 'same_content_pin': True,
            'resolved_versions': True, 'install_does_not_execute': True, 'credential_values_not_persisted': True,
            'integrity_failure_preserves_registry': True, 'cancelled': True,
            'default_feature_rejection': bool(options.disabled_binary), 'real_model_requests': 0}))
    finally:
        server.shutdown()
        server.server_close()
