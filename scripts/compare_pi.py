#!/usr/bin/env python3
"""Manual coding_v1 comparison with installed Pi 1.4.0; defaults to an offline contract fixture.

Build aicoder-bench first. --live explicitly uses one provider from an existing Pi models.json.
Secrets stay in this process; both agents receive only isolated loopback credentials.
"""
import argparse
import hashlib
import http.server
import json
import os
from pathlib import Path
import selectors
import shutil
import signal
import stat
import sys
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
from urllib.parse import urlsplit
import uuid

ROOT = Path(__file__).resolve().parents[1]
MODEL = 'DeepSeek-V4.1-Flash-1017'
LIMITS = {'model_requests': 8, 'tool_calls': 16, 'total_tokens': 32000,
          'output_tokens_per_request': 2048, 'engine_timeout_seconds': 120,
          'upstream_timeout_seconds': 60}
MAX_CAPTURE = 8 * 1024 * 1024
MAX_ARCHIVE = 128 * 1024 * 1024


def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode()


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Archive:
    def __init__(self, path):
        self.root = path.resolve()
        self.root.mkdir(mode=0o700, parents=False, exist_ok=False)
        self.directory = os.open(self.root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        self.bytes = 0
        self.refs = []

    def write(self, name, data):
        if not isinstance(data, bytes):
            data = encoded(data) + b'\n'
        if len(data) > 32 * 1024 * 1024 or self.bytes + len(data) > MAX_ARCHIVE:
            raise RuntimeError('comparison archive budget exhausted; completed work not repeated')
        parts = name.split('/')
        if any(p in ['', '.', '..'] for p in parts) or '\\' in name:
            raise RuntimeError('invalid comparison write path')
        cursor = os.dup(self.directory)
        temporary = '.' + parts[-1] + '.' + uuid.uuid4().hex
        try:
            for part in parts[:-1]:
                try:
                    os.mkdir(part, mode=0o700, dir_fd=cursor)
                except FileExistsError:
                    pass
                next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=cursor)
                os.close(cursor)
                cursor = next_fd
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
                         0o600, dir_fd=cursor)
            with os.fdopen(fd, 'wb') as file:
                file.write(data)
                file.flush()
                os.fsync(file.fileno())
            os.link(temporary, parts[-1], src_dir_fd=cursor, dst_dir_fd=cursor)
        finally:
            try:
                os.unlink(temporary, dir_fd=cursor)
            except FileNotFoundError:
                pass
            os.close(cursor)
        self.bytes += len(data)
        ref = {'path': name, 'bytes': len(data), 'sha256': sha(data)}
        self.refs.append(ref)
        return ref

    def adopt(self, root):
        # Nested coding archives retain their own stronger verifier and admission limits.
        for path in sorted(root.rglob('*')):
            if path.is_symlink() or not (path.is_dir() or path.is_file()):
                raise RuntimeError('invalid nested coding artifact')
            if path.is_file():
                size = path.stat().st_size
                if size > 32 * 1024 * 1024 or self.bytes + size > MAX_ARCHIVE:
                    raise RuntimeError('comparison collection budget exhausted; child archive retained')
                name = str(path.relative_to(self.root))
                data = bounded_read(self.directory, name, size)
                self.refs.append({'path': name, 'bytes': len(data), 'sha256': sha(data)})
                self.bytes += size

    def close(self):
        os.close(self.directory)


def bounded_read(root_fd, name, limit):
    if (not isinstance(name, str) or '\\' in name
            or any(p in ['', '.', '..'] for p in name.split('/'))):
        raise RuntimeError('invalid comparison reference')
    parts = name.split('/')
    cursor = os.dup(root_fd)
    try:
        for part in parts[:-1]:
            next_fd = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=cursor)
            os.close(cursor)
            cursor = next_fd
        fd = os.open(parts[-1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=cursor)
        with os.fdopen(fd, 'rb') as file:
            info = os.fstat(file.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
                raise RuntimeError('comparison file type/size admission')
            data = file.read(limit + 1)
            if len(data) != info.st_size or len(data) > limit:
                raise RuntimeError('comparison file changed/exhausted byte budget')
            return data
    finally:
        os.close(cursor)


def verify_comparison(root):
    """Verify bounded unsigned evidence and summary, never execute an agent or grader."""
    root_fd = os.open(root.resolve(), os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        return verify_from_directory(root_fd)
    finally:
        os.close(root_fd)


def verify_from_directory(root_fd):
    data = bounded_read(root_fd, 'comparison.json', 8 * 1024 * 1024)
    report = json.loads(data)
    refs = report['artifacts']
    if report['schema_version'] != 1 or len(refs) > 256:
        raise RuntimeError('comparison schema/reference budget')
    total = 0
    seen = set()
    for ref in refs:
        name = ref['path']
        if (not isinstance(name, str) or name in seen or '\\' in name
                or any(p in ['', '.', '..'] for p in name.split('/'))):
            raise RuntimeError('invalid comparison reference')
        seen.add(name)
        total += ref['bytes']
        if not 0 <= ref['bytes'] <= 32 * 1024 * 1024 or total > MAX_ARCHIVE:
            raise RuntimeError('comparison artifact admission budget')
        data = bounded_read(root_fd, name, ref['bytes'])
        if len(data) != ref['bytes'] or sha(data) != ref['sha256']:
            raise RuntimeError('comparison artifact fingerprint mismatch')
    by_name = {ref['path']: ref for ref in refs}

    def verified_json(name, limit):
        ref = by_name[name]
        data = bounded_read(root_fd, name, min(limit, ref['bytes']))
        if len(data) != ref['bytes'] or sha(data) != ref['sha256']:
            raise RuntimeError('comparison artifact changed before use')
        return json.loads(data)

    plan = verified_json('plan.json', 1024 * 1024)
    if report['plan'] != plan or 'plan.json' not in seen:
        raise RuntimeError('comparison plan mismatch')
    if not isinstance(plan['selected'], list) or not 1 <= len(plan['selected']) <= 32:
        raise RuntimeError('comparison selection budget')
    expected = {(c, a) for c in plan['selected'] for a in ['aicoder', 'pi']}
    results = report['results']
    if len(results) != len(expected) or {(r['case'], r['agent']) for r in results} != expected:
        raise RuntimeError('comparison case selection mismatch')
    for result in results:
        name = 'cases/' + result['case'] + '/' + result['agent'] + '/result.json'
        if name not in seen or verified_json(name, 1024 * 1024) != result:
            raise RuntimeError('comparison result mismatch')
    summary = {agent: {'selected': len(plan['selected']),
                      'passed': sum(r['passed'] for r in results if r['agent'] == agent)}
               for agent in ['aicoder', 'pi']}
    if summary != report['summary']:
        raise RuntimeError('comparison summary mismatch')
    return report


def run_owned(command, cwd, environment, timeout):
    """Bound pipes, retain the unreaped leader until group signalling, then await cleanup."""
    started = time.monotonic()
    child = subprocess.Popen(command, cwd=cwd, env=environment, stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    buffers = {child.stdout: bytearray(), child.stderr: bytearray()}
    observed = {}
    pending = bytearray()
    overflow = False
    timed_out = False
    signalled = False
    peak_rss = None
    post_signal = None
    try:
        with selectors.DefaultSelector() as selector:
            for pipe in buffers:
                selector.register(pipe, selectors.EVENT_READ)
            while selector.get_map() or not signalled:
                now = time.monotonic()
                if not signalled:
                    status = Path(f'/proc/{child.pid}/status')
                    try:
                        row = next(v for v in status.read_text().splitlines() if v.startswith('VmRSS:'))
                        peak_rss = max(peak_rss or 0, int(row.split()[1]))
                    except (FileNotFoundError, StopIteration):
                        pass
                    exited = os.waitid(os.P_PID, child.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                    if exited is not None or now - started >= timeout:
                        timed_out = exited is None
                        os.killpg(child.pid, signal.SIGKILL)
                        signalled = True
                        post_signal = now
                if signalled and now - post_signal > 2:
                    break
                for key, _ in selector.select(.01):
                    data = os.read(key.fileobj.fileno(), 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                        continue
                    buf = buffers[key.fileobj]
                    room = max(0, MAX_CAPTURE - sum(len(v) for v in buffers.values()))
                    buf.extend(data[:room])
                    overflow |= len(data) > room
                    if key.fileobj == child.stdout:
                        pending.extend(data[:room])
                        while b'\n' in pending:
                            line, _, pending = pending.partition(b'\n')
                            try:
                                event = json.loads(line)
                                if event.get('type') in ['agent_start', 'agent_settled']:
                                    observed.setdefault(event['type'], (time.monotonic() - started) * 1000)
                            except (ValueError, AttributeError):
                                pass
            capture_complete = not selector.get_map() and not overflow
        if not signalled:
            # EOF can precede exit; retain the leader until signalling even on that path.
            os.killpg(child.pid, signal.SIGKILL)
            signalled = True
        code = child.wait(timeout=5)
    finally:
        if child.returncode is None:
            if not signalled:
                os.killpg(child.pid, signal.SIGKILL)
            child.wait(timeout=5)
        child.stdout.close()
        child.stderr.close()
    return {'exit_code': code, 'timed_out': timed_out, 'capture_complete': capture_complete,
            'process_wall_ms': (time.monotonic() - started) * 1000,
            'sampled_root_process_rss_kib': peak_rss, 'event_observed_ms': observed}, bytes(buffers[child.stdout]), bytes(buffers[child.stderr])


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *_args, **_kwargs):
        return None


class Route:
    def __init__(self, case, mode, model, upstream, variant):
        self.case = case
        self.variant = variant
        self.mode = mode
        self.model = model
        self.upstream = upstream
        self.nonce = uuid.uuid4().hex
        self.records = []
        self.tokens = 0
        self.tools = 0
        self.blocked = None
        self.closed = False
        self.deadline = time.monotonic() + LIMITS['engine_timeout_seconds']
        self.lock = threading.Lock()

    def respond(self, value):
        with self.lock:
            if self.closed:
                raise RuntimeError('comparison route closed')
            if len(self.records) >= LIMITS['model_requests'] or time.monotonic() >= self.deadline:
                self.blocked = 'model request/deadline admission exhausted'
            if self.blocked:
                raise RuntimeError('model request/deadline admission exhausted')
            value = dict(value)
            value.update(model=self.model, stream=False, seed=0, temperature=0,
                         max_tokens=LIMITS['output_tokens_per_request'])
            value.pop('stream_options', None)
            value.pop('max_completion_tokens', None)
            value.pop('store', None)
            record = {'request': value}
            self.records.append(record)  # Failed upstream attempts still consume a request.
            started = time.monotonic()
            try:
                if self.mode == 'live':
                    url, key = self.upstream
                    request = urllib.request.Request(url.rstrip('/') + '/chat/completions', data=encoded(value),
                        headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
                    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())
                    with opener.open(request, timeout=max(.1, min(LIMITS['upstream_timeout_seconds'], self.deadline - time.monotonic()))) as response:
                        data = response.read(2 * 1024 * 1024 + 1)
                    if len(data) > 2 * 1024 * 1024:
                        raise RuntimeError('upstream response budget exhausted')
                    result = json.loads(data)
                else:
                    if self.variant == 'provider-error':
                        raise RuntimeError('fixture provider failure')
                    index = len(self.records)
                    if self.variant == 'noop':
                        index = 3
                    names = {v['function']['name'] for v in value['tools']}
                    core = 'read_file' in names
                    path = next(iter(self.case['files']))
                    if index <= 2:
                        write = index == 2
                        repair = self.case['script'][-1]['arguments']
                        content = repair.get('content') or repair['edits'][0]['new_text']
                        tool = ('write_file' if core else 'write') if write else ('read_file' if core else 'read')
                        arguments = {'path': path, **({'content': content} if write else {})}
                        message = {'role': 'assistant', 'content': None, 'tool_calls': [{'id': 'fixture_' + str(index),
                            'type': 'function', 'function': {'name': tool, 'arguments': json.dumps(arguments)}}]}
                    else:
                        message = {'role': 'assistant', 'content': 'Fixture completed.'}
                    result = {'id': 'fixture', 'object': 'chat.completion', 'created': 1, 'model': self.model,
                        'choices': [{'index': 0, 'message': message, 'finish_reason': 'tool_calls' if index <= 2 else 'stop'}],
                        'usage': {'prompt_tokens': 10, 'completion_tokens': 5, 'total_tokens': 15}}
                if self.mode != 'live' and self.variant == 'unknown-usage':
                    result.pop('usage', None)
                if self.mode != 'live' and self.variant == 'tool-limit':
                    result['choices'][0]['message']['tool_calls'] *= 17
                record['response'] = result
                message = result['choices'][0]['message']
                calls = len(message.get('tool_calls', []))
                usage = result.get('usage', {})
                tokens = usage.get('total_tokens')
                if not isinstance(tokens, int) or isinstance(tokens, bool) or tokens < 0:
                    self.blocked = 'unknown usage; no further model request admitted'
                else:
                    self.tokens += tokens
                    if self.tokens > LIMITS['total_tokens']:
                        self.blocked = 'observed token budget exceeded by completed response'
                        raise RuntimeError(self.blocked)
                if self.tools + calls > LIMITS['tool_calls']:
                    self.blocked = 'tool batch admission exhausted'
                    raise RuntimeError(self.blocked)
                self.tools += calls
                return result
            except Exception as error:
                # Never echo upstream exceptions: URLs/headers/server errors can contain secrets.
                record['error'] = {'kind': type(error).__name__, 'status': getattr(error, 'code', None)}
                self.blocked = self.blocked or 'upstream request failed; no implicit retry'
                raise RuntimeError('comparison model request failed') from None
            finally:
                record['http_wall_ms'] = (time.monotonic() - started) * 1000

    def finish(self):
        # Keep usage from an in-flight request even if its frontend has already stopped.
        if not self.lock.acquire(timeout=LIMITS['upstream_timeout_seconds'] + 1):
            raise RuntimeError('model request observation incomplete; no final comparison published')
        try:
            self.closed = True
        finally:
            self.lock.release()


class Proxy(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        route = self.server.routes.get(self.path)
        if route is None or self.headers.get('Authorization') != 'Bearer comparison-loopback':
            self.send_error(403)
            return
        try:
            length = int(self.headers.get('Content-Length', '0'))
            if not 0 < length <= 2 * 1024 * 1024:
                raise RuntimeError('request body budget')
            value = json.loads(self.rfile.read(length))
            streaming = value.get('stream', False)
            result = route.respond(value)
            if streaming:
                first = result['choices'][0]
                delta = json.loads(json.dumps(first['message']))
                for i, call in enumerate(delta.get('tool_calls', [])):
                    call['index'] = i
                common = {k: result[k] for k in ['id', 'created', 'model'] if k in result}
                common['object'] = 'chat.completion.chunk'
                frames = [{**common, 'choices': [{'index': 0, 'delta': delta, 'finish_reason': None}]},
                    {**common, 'choices': [{'index': 0, 'delta': {}, 'finish_reason': first['finish_reason']}],
                     'usage': result.get('usage', {})}]
                body = (''.join('data: ' + json.dumps(v) + '\n\n' for v in frames) + 'data: [DONE]\n\n').encode()
                content_type = 'text/event-stream'
            else:
                body = encoded(result)
                content_type = 'application/json'
            self.send_response(200)
        except Exception:
            body = encoded({'error': {'message': 'Comparison model request rejected', 'type': 'comparison_error'}})
            content_type = 'application/json'
            self.send_response(400)
        self.send_header('Content-Type', content_type)
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def log_message(self, *_):
        pass


def snapshot(root):
    files = {}
    total = 0
    count = 0
    for base, dirs, names in os.walk(root, followlinks=False):
        for name in [*dirs, *names]:
            count += 1
            path = Path(base) / name
            info = path.lstat()
            if count > 20000 or stat.S_ISLNK(info.st_mode):
                raise RuntimeError('workspace scan/symlink rejected')
            if stat.S_ISDIR(info.st_mode):
                continue
            if not stat.S_ISREG(info.st_mode) or info.st_size > 16 * 1024 * 1024:
                raise RuntimeError('workspace file admission rejected')
            with open(path, 'rb') as file:
                data = file.read(16 * 1024 * 1024 + 1)
            if len(data) > 16 * 1024 * 1024:
                raise RuntimeError('workspace changed beyond file admission')
            total += len(data)
            if total > 64 * 1024 * 1024:
                raise RuntimeError('workspace byte budget exhausted')
            files[str(path.relative_to(root))] = data
    return files


def grade(case, files, environment, archive, prefix):
    changed = sorted(k for k in files.keys() | case['files'].keys()
                     if files.get(k) != case['files'].get(k, '').encode())
    if set(changed) - set(case['allowed_changes']):
        return {'passed': False, 'reason': 'unexpected file changes', 'changed': changed}
    grading = case['grading']
    if grading['kind'] == 'json':
        try:
            passed = json.loads(files[grading['path']]) == grading['expected']
        except (KeyError, ValueError):
            passed = False
        return {'passed': passed, 'changed': changed}
    with tempfile.TemporaryDirectory(prefix='aicoder-pi-grader-') as folder:
        root = Path(folder)
        for name, data in files.items():
            path = root / 'candidate' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (root / 'harness.rs').write_text(grading['tests'])
        compile_result, out, err = run_owned(['rustup', 'run', '1.92.0', 'rustc', '--edition=2024',
            '--test', 'harness.rs', '-o', 'tests'], root, environment, 30)
        archive.write(prefix + '/grader-compile.stdout', out)
        archive.write(prefix + '/grader-compile.stderr', err)
        passed = compile_result['exit_code'] == 0 and compile_result['capture_complete']
        test_result = None
        if passed:
            test_result, out, err = run_owned([str(root / 'tests')], root, environment, 5)
            archive.write(prefix + '/grader-test.stdout', out)
            archive.write(prefix + '/grader-test.stderr', err)
            passed = test_result['exit_code'] == 0 and test_result['capture_complete']
        return {'passed': passed, 'changed': changed, 'compile': compile_result, 'tests': test_result}


def pi_package(path):
    executable = Path(path or shutil.which('pi') or '/missing/pi').resolve()
    for parent in [executable, *executable.parents]:
        candidate = parent / 'package.json'
        if candidate.is_file():
            metadata = json.loads(candidate.read_text())
            if metadata.get('name') == '@earendil-works/pi-coding-agent':
                if metadata['version'] != '1.4.0':
                    raise RuntimeError('comparison requires pinned installed Pi 1.4.0')
                entry = parent / metadata['bin']['pi']
                return parent, entry
    raise RuntimeError('installed Pi package not found')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--verify', type=Path)
    parser.add_argument('--bench', type=Path, default=ROOT / 'target/debug/aicoder-bench')
    parser.add_argument('--pi-package', type=Path)
    parser.add_argument('--live', action='store_true')
    parser.add_argument('--fixture-variant', choices=['correct','noop','provider-error','unknown-usage','tool-limit'], default='correct')
    parser.add_argument('--models-config', type=Path)
    parser.add_argument('--provider', default='spark')
    parser.add_argument('--model', default=MODEL)
    options = parser.parse_args()
    if options.verify:
        if options.output or options.live or options.models_config:
            parser.error('--verify cannot execute a comparison')
        report = verify_comparison(options.verify)
        print(json.dumps({'verified': True, 'mode': report['plan']['mode'], 'summary': report['summary']}))
        return 0
    if options.output is None:
        parser.error('--output is required to execute a comparison')
    if bool(options.models_config) != options.live:
        parser.error('--models-config requires --live, and live runs need that explicit file')
    if options.live and options.fixture_variant != 'correct':
        parser.error('live runs cannot select a scripted failure variant')
    _, entry = pi_package(options.pi_package)
    upstream = None
    if options.live:
        with open(options.models_config, 'rb') as config_file:
            raw = config_file.read(1024 * 1024 + 1)
        if len(raw) > 1024 * 1024:
            raise RuntimeError('provider configuration budget exceeded')
        provider = json.loads(raw)['providers'][options.provider]
        if provider['api'] != 'openai-completions' or options.model not in [m['id'] for m in provider['models']]:
            raise RuntimeError('selected model/adapter is not configured')
        key = provider['apiKey']
        if key.startswith('!'):
            raise RuntimeError('command credentials are not executed by this comparison host')
        endpoint = urlsplit(provider['baseUrl'])
        if endpoint.scheme not in ['http', 'https'] or not endpoint.hostname or endpoint.username or endpoint.password or endpoint.query or endpoint.fragment:
            raise RuntimeError('invalid selected provider route')
        upstream = provider['baseUrl'], os.environ.get(key, key)
    if (ROOT / '.aicoder/settings.json').exists():
        raise RuntimeError('comparison source root must not supply project settings')
    suite_bytes = (ROOT / 'crates/aicoder-eval/fixtures/coding-v1.json').read_bytes()
    suite = json.loads(suite_bytes)
    archive = Archive(options.output)
    mode = 'live' if options.live else 'scripted_contract'
    plan = {'schema_version': 1, 'mode': mode, 'model': options.model, 'provider': options.provider,
        'fixture_variant': None if options.live else options.fixture_variant,
        'upstream_route_sha256': sha(upstream[0].encode()) if upstream else None,
        'limits': LIMITS, 'suite_file_sha256': sha(suite_bytes), 'selected': [c['id'] for c in suite['cases']],
        'repetitions': 1, 'upstream_options': {'seed': 0, 'temperature': 0, 'stream': False},
        'source_revision': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=ROOT, text=True).strip(),
        'source_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=ROOT)),
        'bench_sha256': sha(options.bench.read_bytes()), 'pi_package_version': '1.4.0',
        'pi_cli_bundle_sha256': sha(entry.read_bytes()), 'comparison_script_sha256': sha(Path(__file__).read_bytes()),
        'node': subprocess.check_output(['node', '--version'], text=True).strip(),
        'rustc': subprocess.check_output(['rustup', 'run', '1.92.0', 'rustc', '--version'], text=True).strip(),
        'cost': None, 'prices': 'unknown; Pi zero-cost placeholders are not billed cost'}
    archive.write('plan.json', plan)
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Proxy)
    server.routes = {}
    threading.Thread(target=server.serve_forever, daemon=True).start()
    results = []
    try:
        for index, case in enumerate(suite['cases']):
            for agent in (['aicoder', 'pi'] if index % 2 == 0 else ['pi', 'aicoder']):
                prefix = 'cases/' + case['id'] + '/' + agent
                route = Route(case, 'live' if options.live else 'fixture', options.model, upstream, options.fixture_variant)
                url = f'http://127.0.0.1:{server.server_address[1]}/{route.nonce}/v1'
                server.routes['/' + route.nonce + '/v1/chat/completions'] = route
                with tempfile.TemporaryDirectory(prefix='aicoder-pi-case-') as folder:
                    root = Path(folder)
                    workspace = root / 'workspace'
                    workspace.mkdir()
                    for name, content in case['files'].items():
                        path = workspace / name
                        path.parent.mkdir(parents=True, exist_ok=True)
                        path.write_text(content)
                    env = {k: os.environ[k] for k in ['PATH', 'RUSTUP_HOME', 'CARGO_HOME'] if k in os.environ}
                    env.update(AICODER_HOME=str(root / 'state'), COMPARISON_KEY='comparison-loopback',
                               PI_CODING_AGENT_DIR=str(root / 'pi'), PI_OFFLINE='1')
                    if agent == 'aicoder':
                        config = root / 'provider.json'
                        config.write_text(json.dumps({'version': 1, 'profiles': {'comparison': {
                            'base_url': url, 'api_key_env': 'COMPARISON_KEY', 'provider_id': options.provider,
                            'model': options.model, 'proxy': 'disabled', 'max_retries': 0,
                            'models': [{'provider': options.provider, 'id': options.model, 'context_tokens': 240000,
                                'max_output_tokens': 2048, 'capabilities': {'tools': True, 'seed': True,
                                    'temperature': True, 'reasoning_history': True}}]}},
                            'settings': {'provider_profile': 'comparison', 'options': {'temperature': 0}}}))
                        output = archive.root / prefix / 'coding-archive'
                        output.parent.mkdir(parents=True, exist_ok=True)
                        command = [str(options.bench.resolve()), 'run', '--live', '--model', options.model,
                            '--user-config', str(config), '--source-root', str(ROOT), '--output', str(output),
                            '--case', case['id'], '--seed', '0', '--max-rounds', '8', '--max-tool-calls', '16',
                            '--token-budget', '32000', '--time-budget-ms', '120000']
                        capture, out, err = run_owned(command, ROOT, env, 160)
                        verified = subprocess.run([str(options.bench.resolve()), 'verify', str(output)],
                            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15).returncode == 0
                        manifest = json.loads((output / 'manifest.json').read_bytes()) if verified else None
                        case_result = manifest['cases'][0] if manifest else None
                        passed = bool(verified and case_result['verdict'] == 'passed')
                        detail = {'archive_verified': verified, 'case': case_result}
                        archive.adopt(output)
                    else:
                        pi = root / 'pi'
                        pi.mkdir()
                        (pi / 'models.json').write_text(json.dumps({'providers': {options.provider: {
                            'api': 'openai-completions', 'baseUrl': url, 'apiKey': 'comparison-loopback', 'authHeader': True,
                            'models': [{'id': options.model, 'name': options.model, 'reasoning': False,
                                'input': ['text'], 'contextWindow': 240000, 'maxTokens': 2048,
                                'cost': {'input': 0, 'output': 0, 'cacheRead': 0, 'cacheWrite': 0}}]}}}))
                        (pi / 'settings.json').write_text(json.dumps({'quietStartup': True,
                            'retry': {'enabled': False}, 'compaction': {'enabled': False}, 'cacheWarming': 'off',
                            'providerRetry': {'maxRetries': 0}}))
                        command = ['node', str(entry), '--offline', '--no-session', '--no-extensions', '--no-mcp',
                            '--no-skills', '--no-prompt-templates', '--no-context-files', '--no-themes',
                            '--thinking', 'off', '--provider', options.provider, '--model', options.model,
                            '--mode', 'json', '--print', case['prompt']]
                        capture, out, err = run_owned(command, workspace, env, 120)
                        frames = [json.loads(raw) for raw in out.splitlines()] if capture['capture_complete'] else []
                        messages = [f['message'] for f in frames if f.get('type') == 'message_end' and f['message'].get('role') == 'assistant']
                        complete = (any(f.get('type') == 'agent_settled' and not f.get('aborted') for f in frames)
                                    and messages and messages[-1].get('stopReason') == 'stop')
                        files = snapshot(workspace)
                        for name, data in files.items():
                            archive.write(prefix + '/workspace/' + name, data)
                        grading = grade(case, files, env, archive, prefix)
                        passed = bool(complete and grading['passed'])
                        detail = {'engine_completed': bool(complete), 'grading': grading,
                            'tool_failures': sum(f.get('type') == 'tool_execution_end' and bool(f.get('isError')) for f in frames)}
                    route.finish()
                    archive.write(prefix + '/stdout', out)
                    archive.write(prefix + '/stderr', err)
                    archive.write(prefix + '/provider.json', route.records)
                    passed = bool(passed and capture['exit_code'] == 0 and capture['capture_complete']
                        and not capture['timed_out'] and not route.blocked)
                    result = {'case': case['id'], 'agent': agent, 'passed': passed, 'capture': capture,
                        'model_requests': len(route.records), 'model_tool_calls': route.tools,
                        'provider_total_tokens': route.tokens if all('response' in r and type(r['response'].get('usage', {}).get('total_tokens')) is int for r in route.records) else None,
                        'provider_http_wall_ms': sum(r['http_wall_ms'] for r in route.records),
                        'admission_stop': route.blocked, 'detail': detail, 'cost': None}
                    results.append(result)
                    archive.write(prefix + '/result.json', result)
                    print(json.dumps({k: result[k] for k in ['case', 'agent', 'passed', 'model_requests', 'provider_total_tokens']}), flush=True)
                del server.routes['/' + route.nonce + '/v1/chat/completions']
        summary = {agent: {'selected': len(suite['cases']), 'passed': sum(r['passed'] for r in results if r['agent'] == agent)} for agent in ['aicoder', 'pi']}
        report = {'schema_version': 1, 'plan': plan, 'summary': summary, 'results': results, 'artifacts': archive.refs.copy(),
            'classification': 'manual real-service small coding smoke; not SWE-bench or general coding equivalence' if options.live else 'scripted integration contract; no model quality claim',
            'limits': ['The proxy converts complete upstream JSON to Pi SSE; transport latency/streaming behavior is not compared.',
                'Budgets admit at most eight model attempts and sixteen response tool calls. Observed token usage is checked after each completed response; the last charged request can exceed the cap. Unknown usage stops further requests.',
                'System prompts, tools and agent implementation differ. Identical upstream options do not guarantee deterministic or equally tokenized execution.',
                'Aicoder agent duration is core time; Pi agent duration is observed stdout event delivery. Process wall time includes different startup/grading/archive work.',
                'Root-process RSS is sampled at ten-millisecond intervals through process lifetime; children/proxy/grading outside that process are excluded. It is not exact peak or isolated agent memory.',
                'Grading is hidden after execution but is not an OS sandbox. Process groups do not contain detached descendants.',
                'One sample per tiny case cannot establish general success rate, full Pi parity or billed cost.']}
        archive.write('comparison.json', report)
        print(json.dumps({'mode': mode, 'summary': summary, 'cost': None}), flush=True)
        return 0 if all(r['passed'] for r in results) else 1
    finally:
        server.shutdown()
        server.server_close()
        archive.close()


if __name__ == '__main__':
    sys.exit(main())
