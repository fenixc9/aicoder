#!/usr/bin/env python3
"""Offline CLI dependency-source closure and concurrent-registry transaction checks."""
import argparse
import base64
import gzip
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import threading

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/aicoder')
binary = parser.parse_args().binary.resolve()

with tempfile.TemporaryDirectory(prefix='aicoder-package-graph-') as folder:
    root = Path(folder)
    workspace = root / 'workspace'
    workspace.mkdir()
    env = {'PATH': os.environ.get('PATH', ''), 'AICODER_HOME': str(root / 'state')}
    prefix = [str(binary), '--no-config', '--no-resources', '--workspace', str(workspace), 'packages']

    def cli(*arguments, environment=env):
        return subprocess.run([*prefix, *arguments], env=environment, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, timeout=15)

    def bundle(name, dependencies):
        source = root / name
        source.mkdir()
        manifest = {'schema_version': 1, 'id': name, 'version': '1.0.0', 'files': ['rules.md'],
                    'resources': {'instructions': ['rules.md']}, 'permissions': {'instructions': True},
                    'dependencies': dependencies}
        (source / 'aicoder-package.json').write_text(json.dumps(manifest))
        (source / 'package.json').write_text(json.dumps({'name': '@scope/' + name, 'version': '1.0.0'}))
        (source / 'rules.md').write_text(name + ' rules')
        record = cli('install', str(source), '--disabled', environment={**env, 'AICODER_HOME': str(root / 'reference')})
        assert record.returncode == 0, record.stderr
        data = io.BytesIO()
        with tarfile.open(fileobj=data, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for file in source.iterdir():
                archive.add(file, arcname='package/' + file.name)
        return json.loads(record.stdout)['pin'], gzip.compress(data.getvalue())

    leaf, leaf_bytes = bundle('leaf', [])
    middle, middle_bytes = bundle('middle', [leaf])
    top, top_bytes = bundle('root', [middle])
    bodies = {'leaf': leaf_bytes, 'middle': middle_bytes, 'root': top_bytes}
    entered, release = threading.Event(), threading.Event()
    requests = []

    class Registry(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            requests.append(self.path)
            if self.path.endswith('/race'):
                entered.set()
                assert release.wait(10)
            name = self.path.split('/')[-1] if self.path.startswith('/archive/') else self.path.split('/')[-2].split('%2F')[-1]
            body = bodies[name]
            if not self.path.startswith('/archive/'):
                body = json.dumps({'name': '@scope/' + name, 'version': '1.0.0', 'dist': {
                    'tarball': f'http://127.0.0.1:{self.server.server_port}/archive/{name}',
                    'integrity': 'sha512-' + base64.b64encode(hashlib.sha512(body).digest()).decode()}}).encode()
            self.send_response(200)
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Registry)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        url = f'http://127.0.0.1:{server.server_port}'
        catalogue = root / 'sources.json'
        catalogue.write_text(json.dumps({'schema_version': 1, 'entries': [{'pin': pin, 'request': {
            'kind': 'npm', 'registry': url, 'package': '@scope/' + pin['id'], 'version': '1.0.0'}}
            for pin in [leaf, middle]]}))
        arguments = ['install-npm', '@scope/root', '--registry', url, '--allow-loopback-http',
                     '--source-catalogue', str(catalogue)]
        result = cli(*arguments)
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)['pin'] == top
        listed = json.loads(cli('list').stdout)
        assert listed['revision'] == 1 and listed['enabled'] == ['root'] and len(listed['installed']) == 3
        assert len(requests) == 6
        registry = root / 'state/packages/registry.json'
        before = registry.read_bytes()
        # A source identity mismatch cannot create a partially selected dependency graph.
        bad = json.loads(catalogue.read_text())
        bad['entries'][0]['pin']['sha256'] = 'f' * 64
        bad_path = root / 'bad-sources.json'
        bad_path.write_text(json.dumps(bad))
        failed = cli(*[str(bad_path) if v == str(catalogue) else v for v in arguments],
                     environment={**env, 'AICODER_HOME': str(root / 'failed-state')})
        assert failed.returncode != 0
        failed_index = json.loads(cli('list', environment={**env, 'AICODER_HOME': str(root / 'failed-state')}).stdout)
        assert not failed_index['installed'] and failed_index['revision'] == 0
        assert registry.read_bytes() == before
        child = subprocess.Popen([*prefix, *arguments, '--version', 'race'], env=env,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            assert entered.wait(10)
            changed = cli('disable', 'root')
            assert changed.returncode == 0
            concurrent = registry.read_bytes()
            release.set()
            stdout, stderr = child.communicate(timeout=15)
            assert child.returncode != 0 and not stdout and b'registry changed' in stderr
            assert registry.read_bytes() == concurrent
        finally:
            release.set()
            if child.poll() is None:
                child.kill()
                child.wait()
        assert not list((root / 'state/packages/staging').iterdir())
        print(json.dumps({'transitive_packages': 3, 'one_registry_commit': True,
            'only_root_enabled': True, 'wrong_pin_no_partial_publication': True,
            'concurrent_writer_preserved': True, 'staging_cleaned': True, 'real_model_requests': 0}))
    finally:
        release.set()
        server.shutdown()
        server.server_close()
