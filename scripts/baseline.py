#!/usr/bin/env python3
"""Linux development-only baseline collector; no model credentials or network calls."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def capture(command, **kwargs):
    return subprocess.run(command, cwd=ROOT, check=True, text=True,
                          stdout=subprocess.PIPE, **kwargs).stdout


def measured(command):
    with tempfile.NamedTemporaryFile() as metrics:
        started = time.perf_counter()
        output = capture(["/usr/bin/time", "-f", "%M", "-o", metrics.name, *command])
        elapsed_ms = (time.perf_counter() - started) * 1000
        metrics.seek(0)
        rss_kib = int(metrics.read().decode().strip())
    return {"wall_ms_including_time_wrapper": elapsed_ms, "peak_rss_kib": rss_kib}, output


def source_digest():
    paths = [ROOT / "Cargo.toml", ROOT / "Cargo.lock", ROOT / "rust-toolchain.toml"]
    # Embedded Web assets and benchmark JSON fixtures affect binaries like Rust sources.
    for pattern in ("*.rs", "*.html", "*.js", "*.css", "*.json", "*.py"):
        paths += list((ROOT / "crates").rglob(pattern))
    paths += list((ROOT / "crates").rglob("Cargo.toml"))
    digest = hashlib.sha256()
    for path in sorted(paths):
        digest.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    return digest.hexdigest()


def collect(output, clean_build, samples, iterations):
    output.mkdir(parents=True, exist_ok=True)
    metadata = json.loads(capture(["cargo", "metadata", "--locked", "--format-version", "1"]))
    # Strip machine-specific paths; keep exact package versions and dependency edges.
    dependencies = {
        "packages": [{key: p.get(key) for key in ("id", "name", "version", "source", "rust_version")}
                     for p in metadata["packages"]],
        "resolve": metadata["resolve"],
    }
    (output / "dependencies.json").write_text(
        json.dumps(dependencies, indent=2).replace(ROOT.as_uri(), "workspace:") + "\n")
    (output / "dependencies.txt").write_text(capture(["cargo", "tree", "--locked", "--workspace"]))
    build_env = os.environ.copy()
    # A fresh target directory measures clean compilation without deleting user build output.
    with tempfile.TemporaryDirectory(prefix="aicoder-baseline-") as fresh:
        if clean_build:
            build_env["CARGO_TARGET_DIR"] = fresh
        target_dir = Path(fresh) if clean_build else Path(metadata["target_directory"])
        started = time.perf_counter()
        subprocess.run(["cargo", "build", "--locked", "--release", "--workspace", "--bins", "--examples"],
                       cwd=ROOT, env=build_env, check=True)
        build_seconds = time.perf_counter() - started
        release = target_dir / "release"
        executables = [release / name for name in ("aicoder", "aicoder-tui", "aicoder-swebench")]
        binaries = {}
        for binary in executables:
            first, _ = measured([str(binary), "--help"])
            warm = [measured([str(binary), "--help"])[0] for _ in range(samples)]
            binaries[binary.name] = {
                "bytes": binary.stat().st_size,
                "first_process_help": first,
                "warm_process_help_median_ms": statistics.median(s["wall_ms_including_time_wrapper"] for s in warm),
                "warm_process_help_samples": warm,
            }
        smoke_binary = release / "examples" / "offline_smoke"
        smoke_bytes = smoke_binary.stat().st_size
        runtime, smoke_output = measured([str(smoke_binary), "--iterations", str(iterations)])
        smoke = json.loads(smoke_output)
        (output / "offline-smoke.json").write_text(json.dumps(smoke, indent=2) + "\n")
        # This is runtime-only idle RSS, not the memory of a rendered interactive TUI.
        idle = subprocess.Popen([str(smoke_binary), "--idle-ms", "1000"], cwd=ROOT,
                                text=True, stdout=subprocess.PIPE)
        try:
            time.sleep(0.2)
            status = Path(f"/proc/{idle.pid}/status").read_text()
            idle_rss = int(next(line.split()[1] for line in status.splitlines() if line.startswith("VmRSS:")))
            stdout, _ = idle.communicate(timeout=30)
            if idle.returncode:
                raise RuntimeError(f"idle smoke failed: {idle.returncode}")
            json.loads(stdout)
        finally:
            if idle.poll() is None:
                idle.kill()
                idle.wait()
    report = {
        "schema_version": 1,
        "revision": capture(["git", "rev-parse", "HEAD"]).strip(),
        "worktree_dirty": bool(capture(["git", "status", "--porcelain"]).strip()),
        "source_sha256": source_digest(),
        "platform": platform.platform(), "machine": platform.machine(), "logical_cpus": os.cpu_count(),
        "rustc": capture(["rustc", "--version"]).strip(),
        "cargo": capture(["cargo", "--version"]).strip(),
        "build": {"mode": "clean-target" if clean_build else "existing-target", "seconds": build_seconds},
        "binaries": binaries,
        "runtime": {**runtime, "idle_runtime_rss_kib": idle_rss, "iterations": iterations,
                    "cases_per_iteration": 5, "example_bytes": smoke_bytes},
        "limitations": [
            "No filesystem/OS page caches are cleared: first process is not disk-cold startup.",
            "Help includes process creation and GNU time wrapper overhead, not interactive startup.",
            "Idle RSS samples the current-thread offline runtime, not the TUI.",
            "Smoke uses a scripted provider; it measures no real model latency or task success rate.",
            "No allocator or message-clone instrumentation is performed; transcript bytes are serialized size only.",
        ],
    }
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps({"report": str(output / "report.json"), "smoke_cases": len(smoke["cases"])}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=ROOT / "artifacts" / "baseline")
    parser.add_argument("--clean-build", action="store_true")
    parser.add_argument("--samples", type=int, default=10)
    parser.add_argument("--iterations", type=int, default=25)
    args = parser.parse_args()
    if platform.system() != "Linux":
        parser.error("baseline collection currently requires Linux /proc and GNU time")
    if args.samples < 1 or args.iterations < 1:
        parser.error("samples and iterations must be positive")
    collect(args.output.resolve(), args.clean_build, args.samples, args.iterations)
