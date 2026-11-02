"""Linux-only execution inputs and namespaces for the optional manual Pi comparison."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile

MAX_ENTRIES = 32768
MAX_BYTES = 512 * 1024 * 1024


def manifest_digest(entries):
    digest = hashlib.sha256()
    size = 0
    encoder = json.JSONEncoder(sort_keys=True, separators=(',', ':'))
    for part in encoder.iterencode(entries):
        data = part.encode()
        size += len(data)
        if size > 4 * 1024 * 1024:
            raise RuntimeError('executable manifest serialization budget exhausted')
        digest.update(data)
    return digest.hexdigest()


def fingerprint(data):
    return hashlib.sha256(data).hexdigest()


def file_fingerprint(path, limit=32 * 1024 * 1024):
    digest = hashlib.sha256()
    with open(path, 'rb') as file:
        info = os.fstat(file.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise RuntimeError('execution helper fingerprint admission exhausted')
        size = 0
        while True:
            block = file.read(min(65536, limit + 1 - size))
            if not block:
                break
            size += len(block)
            if size > limit:
                raise RuntimeError('execution helper grew beyond fingerprint budget')
            digest.update(block)
        after = os.fstat(file.fileno())
    if (size != info.st_size or info.st_mtime_ns != after.st_mtime_ns
            or info.st_ctime_ns != after.st_ctime_ns):
        raise RuntimeError('execution helper changed during fingerprinting')
    return digest.hexdigest()


class IsolatedExecutables:
    """Freeze application code outside model-visible filesystems; never copy user config."""

    def __init__(self, bench, package):
        self.temporary = tempfile.TemporaryDirectory(prefix='aicoder-comparison-code-')
        self.root = Path(self.temporary.name)
        self.entries = []
        self.bytes = 0
        self.bwrap = Path(shutil.which('bwrap') or '/missing/bwrap').resolve()
        self.bwrap_sha256 = file_fingerprint(self.bwrap)
        try:
            self._file(bench.resolve(), self.root / 'aicoder-bench', 256 * 1024 * 1024)
            self._file(Path(shutil.which('node') or '/missing/node').resolve(), self.root / 'node',
                       256 * 1024 * 1024)
            self._package(package.resolve(), self.root / 'pi')
            metadata = json.loads((self.root / 'pi/package.json').read_bytes())
            if (metadata.get('name') != '@earendil-works/pi-coding-agent'
                    or metadata.get('version') != '1.4.0'):
                raise RuntimeError('frozen Pi installation changed identity/version')
            entry = self.root / 'pi' / metadata['bin']['pi']
            if not entry.resolve(strict=True).is_relative_to(self.root / 'pi'):
                raise RuntimeError('frozen Pi entry escapes its package')
            self.pi_entry = entry.relative_to(self.root / 'pi')
            compiler = subprocess.check_output(['rustup', 'which', '--toolchain', '1.92.0', 'rustc'],
                                               text=True, timeout=10).strip()
            self.toolchain = Path(compiler).resolve().parent.parent
            self.compiler_sha256 = file_fingerprint(self.toolchain / 'bin/rustc')
            probe = subprocess.run(self.wrap(['/bin/sh', '-c',
                "printf 'fn main() {}' > /tmp/probe.rs && rustc /tmp/probe.rs -o /tmp/probe && /tmp/probe"], {}, []), timeout=10,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            if probe.returncode:
                raise RuntimeError('isolated benchmark startup unavailable: '
                                   + probe.stderr.decode(errors='replace')[:2048])
        except BaseException:
            self.close()
            raise

    def _admit(self, size):
        if len(self.entries) >= MAX_ENTRIES or size > MAX_BYTES - self.bytes:
            raise RuntimeError('frozen executable package admission exhausted')
        if shutil.disk_usage(self.root).free < size + 64 * 1024 * 1024:
            raise RuntimeError('insufficient disk admission for executable snapshot')

    def _file(self, source, target, limit):
        before = source.lstat()
        if not stat.S_ISREG(before.st_mode) or before.st_size > limit:
            raise RuntimeError('invalid executable package file type/size')
        self._admit(before.st_size)
        target.parent.mkdir(parents=True, exist_ok=True)
        digest = hashlib.sha256()
        copied = 0
        with open(source, 'rb') as file, open(target, 'xb') as destination:
            while True:
                block = file.read(min(65536, before.st_size + 1 - copied))
                if not block:
                    break
                copied += len(block)
                if copied > before.st_size:
                    raise RuntimeError('executable package grew during capture')
                digest.update(block)
                destination.write(block)
            after = os.fstat(file.fileno())
        stamp = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns, s.st_mode)
        if (copied != before.st_size or stamp(before) != stamp(after)
                or stamp(before) != stamp(source.lstat())):
            raise RuntimeError('executable package changed during capture')
        target.chmod(0o555 if before.st_mode & 0o111 else 0o444)
        self.bytes += copied
        self.entries.append({'path': str(target.relative_to(self.root)), 'kind': 'file',
                             'bytes': copied, 'sha256': digest.hexdigest(),
                             'executable': bool(before.st_mode & 0o111)})

    def _package(self, source, target):
        before = self._inventory(source)
        self._admit(0)
        target.mkdir(parents=True, exist_ok=True)
        self.entries.append({'path': str(target.relative_to(self.root)), 'kind': 'directory'})
        for name, info in before:
            path = source / name
            destination = target / name
            if stat.S_ISDIR(info.st_mode):
                self._admit(0)
                destination.mkdir(parents=True, exist_ok=True)
                self.entries.append({'path': str(destination.relative_to(self.root)), 'kind': 'directory'})
            elif stat.S_ISLNK(info.st_mode):
                resolved = path.resolve(strict=True)
                if not resolved.is_relative_to(source):
                    raise RuntimeError('package symlink escapes pinned installation')
                link = os.path.relpath(resolved, path.parent)
                self._admit(len(link.encode()))
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.symlink_to(link)
                self.entries.append({'path': str(destination.relative_to(self.root)), 'kind': 'symlink',
                                     'target': link})
                self.bytes += len(link.encode())
            else:
                self._file(path, destination, 16 * 1024 * 1024)
        after = self._inventory(source)
        stamp = lambda rows: [(n, i.st_mode, i.st_ino, i.st_size, i.st_mtime_ns, i.st_ctime_ns) for n, i in rows]
        if stamp(before) != stamp(after):
            raise RuntimeError('installed Pi package changed before freezing completed')

    @staticmethod
    def _inventory(source):
        rows = []
        for base, dirs, files in os.walk(source, followlinks=False):
            for name in [*dirs, *files]:
                path = Path(base) / name
                relative = str(path.relative_to(source))
                if (len(rows) >= MAX_ENTRIES or len(path.relative_to(source).parts) > 64
                        or len(relative.encode()) > 1024):
                    raise RuntimeError('executable inventory admission exhausted')
                if name == '.env':
                    raise RuntimeError('private environment file in executable package')
                info = path.lstat()
                if not any(test(info.st_mode) for test in [stat.S_ISREG, stat.S_ISDIR, stat.S_ISLNK]):
                    raise RuntimeError('special file in executable package')
                rows.append((relative, info))
        return sorted(rows)

    def describe(self):
        entries = sorted(self.entries, key=lambda row: row['path'])
        return {'scope': 'frozen full Pi installation and Node/aicoder executable copies',
                'entries': entries, 'bytes': self.bytes, 'sha256': manifest_digest(entries),
                'compiler': {'version': '1.92.0', 'rustc_sha256': self.compiler_sha256,
                             'scope': 'host pinned toolchain mounted read-only; not a complete system-library pin'},
                'isolation': {'backend': 'bubblewrap', 'executable_sha256': self.bwrap_sha256,
                              'pid_namespace': True, 'private_tmp': 'dedicated tmpfs, kernel-default size',
                              'network': 'shared host network for explicit loopback proxy',
                              'root_process_rss_scope': 'bubblewrap launcher; not agent RSS'}}

    def wrap(self, command, environment, mounts, cwd='/tmp', engine=None):
        if file_fingerprint(self.bwrap) != self.bwrap_sha256:
            raise RuntimeError('isolation executable changed before launch')
        if file_fingerprint(self.toolchain / 'bin/rustc') != self.compiler_sha256:
            raise RuntimeError('grading compiler changed before launch')
        args = [str(self.bwrap), '--unshare-all', '--share-net', '--die-with-parent', '--new-session',
                '--cap-drop', 'ALL', '--clearenv']
        for path in ['/usr', '/bin', '/lib', '/lib64']:
            if Path(path).exists():
                args.extend(['--ro-bind', path, path])
        for path in ['/etc/passwd', '/etc/group', '/etc/nsswitch.conf']:
            if Path(path).is_file():
                args.extend(['--ro-bind', path, path])
        if Path('/etc/alternatives').is_dir():
            args.extend(['--ro-bind', '/etc/alternatives', '/etc/alternatives'])
        args.extend(['--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp',
                     '--dir', '/app', '--dir', '/workspace',
                     '--ro-bind', str(self.toolchain), '/opt/toolchain',
                     '--ro-bind', str(self.root / 'node'), '/opt/node'])
        if engine == 'aicoder':
            args.extend(['--ro-bind', str(self.root / 'aicoder-bench'), '/opt/aicoder-bench'])
        elif engine == 'pi':
            args.extend(['--ro-bind', str(self.root / 'pi'), '/opt/pi'])
        for source, destination, writable in mounts:
            args.extend(['--bind' if writable else '--ro-bind', str(source), destination])
        env = {'PATH': '/opt/toolchain/bin:/opt:/usr/bin:/bin', 'AICODER_HOME': '/tmp/aicoder-state',
               'CARGO_HOME': '/tmp/cargo', 'PI_OFFLINE': '1', **environment}
        for name, value in env.items():
            args.extend(['--setenv', name, value])
        args.extend(['--remount-ro', '/', '--chdir', cwd, '--', *command])
        return args

    def close(self):
        self.temporary.cleanup()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()
