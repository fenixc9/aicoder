#!/usr/bin/env python3
"""Offline namespace/package-pin checks; no actual Pi service or model credentials."""
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import uuid

from compare_pi import ROOT, run_owned
from pi_comparison_isolation import IsolatedExecutables


def process(pid):
    try:
        value = Path(f'/proc/{pid}/stat').read_text().split(') ', 1)[1].split()
        return value[0], value[19]  # State and start time, not reusable signalling authority.
    except FileNotFoundError:
        return None


def descendants(pid):
    found = []
    pending = [pid]
    while pending:
        parent = pending.pop()
        if len(found) >= 64:
            raise RuntimeError('fixture process observation budget')
        try:
            children = Path(f'/proc/{parent}/task/{parent}/children').read_text().split()
        except FileNotFoundError:
            continue
        for child in children:
            child = int(child)
            identity = process(child)
            if identity:
                found.append((child, identity))
                pending.append(child)
    return found


with tempfile.TemporaryDirectory(prefix='aicoder-isolation-check-') as folder:
    root = Path(folder)
    package = root / 'package'
    package.mkdir()
    (package / 'package.json').write_text(json.dumps({'name': '@earendil-works/pi-coding-agent',
        'version': '1.4.0', 'bin': {'pi': 'cli.js'}}))
    (package / 'cli.js').write_text('console.log("fixture");')
    (package / 'asset').write_text('original')
    (package / 'link').symlink_to('asset')
    (package / 'empty').mkdir()
    secret = root / 'not-mounted'
    secret.write_text('fixture private value')
    workspace = root / 'workspace'
    workspace.mkdir()
    with IsolatedExecutables(ROOT / 'target/debug/aicoder-bench', package) as frozen:
        (package / 'asset').write_text('changed after freezing')
        assert (frozen.root / 'pi/asset').read_text() == 'original'
        manifest = frozen.describe()
        assert any(e['path'] == 'pi/empty' and e['kind'] == 'directory' for e in manifest['entries'])
        marker = 'aicoder-private-tmp-' + uuid.uuid4().hex
        code = '''import json, os, pathlib, sys
assert not pathlib.Path(sys.argv[1]).exists()
assert not pathlib.Path('/proc/' + sys.argv[2]).exists()
assert pathlib.Path('/opt/pi/link').read_text() == 'original'
for name in ['/outside-root', '/opt/pi/asset']:
    try:
        pathlib.Path(name).write_text('forbidden')
    except OSError:
        pass
    else:
        raise AssertionError('read-only mount was writable')
pathlib.Path('/tmp/' + sys.argv[3]).write_text('private')
pathlib.Path('/workspace/allowed').write_text('owned case')
print(json.dumps({'private_tmp': True, 'read_only': True, 'host_paths_hidden': True}))
'''
        command = frozen.wrap(['/usr/bin/python3', '-c', code, str(secret), str(os.getpid()), marker],
            {}, [(workspace, '/workspace', True)], engine='pi')
        capture, out, _ = run_owned(command, ROOT, {}, 10)
        assert capture['exit_code'] == 0 and capture['capture_complete'], capture
        assert json.loads(out)['read_only']
        assert (workspace / 'allowed').read_text() == 'owned case'
        assert not Path('/tmp', marker).exists()
        command = frozen.wrap(['/usr/bin/python3', '-c',
            'import pathlib,sys; assert not pathlib.Path("/tmp/"+sys.argv[1]).exists()', marker], {}, [])
        capture, _, _ = run_owned(command, ROOT, {}, 10)
        assert capture['exit_code'] == 0
        observed = []

        def started(pid):
            end = time.monotonic() + 3
            while time.monotonic() < end:
                children = descendants(pid)
                if len(children) >= 3 and (workspace / 'detached-ready').exists():
                    observed.extend(children)
                    return
                time.sleep(.01)
            raise RuntimeError('detached fixture did not start')

        code = '''import os,pathlib,time
if os.fork() == 0:
    os.setsid()
    pathlib.Path('/workspace/detached-ready').write_text('ready')
    time.sleep(30)
else:
    time.sleep(30)
'''
        command = frozen.wrap(['/usr/bin/python3', '-c', code], {}, [(workspace, '/workspace', True)])
        capture, _, _ = run_owned(command, ROOT, {}, .5, on_started=started)
        assert capture['timed_out'] and observed
        end = time.monotonic() + 3
        while time.monotonic() < end:
            if all((now := process(pid)) is None or now[1] != identity[1] or now[0] == 'Z'
                   for pid, identity in observed):
                break
            time.sleep(.01)
        else:
            raise AssertionError('namespace descendant still executing after owner cancellation')
        # This is a liveness check; adopted zombies may await the host init's reaper.
    for variant in ['outside-link', 'private-env']:
        if variant == 'outside-link':
            (package / 'escaped').symlink_to(secret)
        else:
            (package / 'escaped').unlink()
            (package / '.env').write_text('fixture-only')
        try:
            with IsolatedExecutables(ROOT / 'target/debug/aicoder-bench', package):
                raise AssertionError('invalid package was accepted')
        except RuntimeError:
            pass
    print(json.dumps({'private_tmp_between_runs': True, 'read_only_system_and_code': True,
        'host_private_paths_hidden': True, 'only_case_mount_writable': True,
        'frozen_package_ignores_source_edits': True, 'directories_and_links_pinned': True,
        'escaping_links_and_env_rejected': True, 'detached_namespace_descendants_stopped': True,
        'real_model_requests': 0}))
