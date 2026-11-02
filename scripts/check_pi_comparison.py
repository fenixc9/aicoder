#!/usr/bin/env python3
"""Optional installed-Pi integration check. No remote provider or real credential is used."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--output', type=Path)
options = parser.parse_args()
spec = importlib.util.spec_from_file_location('comparison', ROOT / 'scripts/compare_pi.py')
comparison = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison)
environment = {k: os.environ[k] for k in ['PATH', 'RUSTUP_HOME', 'CARGO_HOME'] if k in os.environ}


def reject(call):
    try:
        call()
    except (RuntimeError, OSError, KeyError, ValueError):
        return
    raise AssertionError('invalid evidence/admission was accepted')


with tempfile.TemporaryDirectory(prefix='aicoder-pi-check-') as folder:
    root = options.output.resolve() if options.output else Path(folder)
    root.mkdir(parents=True, exist_ok=True)
    for variant in ['correct', 'noop', 'provider-error', 'unknown-usage', 'tool-limit']:
        archive = root / variant
        with open(root / (variant + '.log'), 'wb') as log:
            result = subprocess.run([sys.executable, str(ROOT / 'scripts/compare_pi.py'),
                '--output', str(archive), '--fixture-variant', variant], cwd=ROOT,
                env=environment, stdout=log, stderr=subprocess.STDOUT, timeout=90)
        assert result.returncode == (0 if variant == 'correct' else 1), variant
        report = comparison.verify_comparison(archive)
        assert len(report['results']) == 8
        for agent in ['pi', 'aicoder']:
            assert report['summary'][agent] == {'selected': 4, 'passed': 4 if variant == 'correct' else 0}
        for result in report['results']:
            assert result['model_requests'] <= 8 and result['model_tool_calls'] <= 16
            if variant in ['unknown-usage', 'provider-error']:
                assert result['provider_total_tokens'] is None
            records = json.loads((archive / 'cases' / result['case'] / result['agent'] / 'provider.json').read_bytes())
            for record in records:
                assert not any(k.lower() == 'authorization' for k in record['request'])
                assert record['request']['seed'] == 0
                assert record['request']['temperature'] == 0
                assert record['request']['max_tokens'] == 2048
        if variant == 'correct':
            fixture = report['plan']['selected'][0]
            source = archive / f'cases/{fixture}/pi/result.json'
            original = source.read_bytes()
            source.write_bytes(original + b' ')
            reject(lambda: comparison.verify_comparison(archive))
            source.write_bytes(original)
            private = root / 'outside'
            private.write_bytes(original)
            source.unlink()
            source.symlink_to(private)
            reject(lambda: comparison.verify_comparison(archive))
            source.unlink()
            os.mkfifo(source)
            reject(lambda: comparison.verify_comparison(archive))
            source.unlink()
            source.write_bytes(original)
            assert comparison.verify_comparison(archive)['summary'] == report['summary']
            # Observation verification does not need Pi, Rust, credentials or a provider.
            checked = subprocess.run([sys.executable, str(ROOT / 'scripts/compare_pi.py'),
                '--verify', str(archive)], env={'PATH': '/missing'}, stdout=subprocess.PIPE, timeout=5)
            assert checked.returncode == 0
    writer = comparison.Archive(root / 'atomic')
    try:
        writer.write('present', b'old')
        reject(lambda: writer.write('present', b'new'))
        assert (writer.root / 'present').read_bytes() == b'old'
        writer.bytes = comparison.MAX_ARCHIVE
        reject(lambda: writer.write('rejected', b'x'))
        assert not (writer.root / 'rejected').exists()
    finally:
        writer.close()
    suite = json.loads((ROOT / 'crates/aicoder-eval/fixtures/coding-v1.json').read_bytes())
    route = comparison.Route(suite['cases'][0], 'fixture', 'fixture', None, 'correct')
    request = {'tools': [{'function': {'name': 'read_file'}}, {'function': {'name': 'write_file'}}]}
    for _ in range(8):
        route.respond(request)
    reject(lambda: route.respond(request))
    assert len(route.records) == 8 and route.blocked
    comparison.MAX_CAPTURE = 1024
    capture, out, err = comparison.run_owned([sys.executable, '-c',
        'import os; os.write(1,b"x"*2097152); os.write(2,b"y"*2097152)'], ROOT, environment, 10)
    assert capture['exit_code'] == 0 and not capture['capture_complete']
    assert len(out) + len(err) <= 1024
    capture, _, _ = comparison.run_owned([sys.executable, '-c',
        'import os,time; os.close(1); os.close(2); time.sleep(.1)'], ROOT, environment, 5)
    assert capture['exit_code'] == 0 and capture['capture_complete']
    capture, _, _ = comparison.run_owned([sys.executable, '-c', 'import time; time.sleep(30)'],
        ROOT, environment, .05)
    assert capture['timed_out'] and capture['exit_code'] != 0
    print(json.dumps({'cohorts': 5, 'selected_per_agent': 4, 'all_failures_retained': True,
        'budget_admission': True, 'unknown_usage': True, 'corrupt_symlink_fifo_rejected': True,
        'atomic_no_replace': True, 'bounded_drained_capture': True, 'owned_timeout': True,
        'verification_without_executables': True, 'real_model_requests': 0}))
