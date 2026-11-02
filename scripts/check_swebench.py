#!/usr/bin/env python3
"""Offline SWE generation/grading contracts. Optional explicit official Python + local fixture image.
Build first: cargo build --locked -p aicoder-eval --bins --example offline_swebench
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/aicoder-swebench")
parser.add_argument("--example", type=Path, default=ROOT / "target/debug/examples/offline_swebench")
parser.add_argument("--output", type=Path)
parser.add_argument("--official-python", type=Path)
parser.add_argument("--image", help="Explicit local immutable image ID from the supplied Docker fixture")
args = parser.parse_args()
assert bool(args.official_python) == bool(args.image), "Official checks require both --official-python and --image"
if args.image:
    assert args.image.startswith("sha256:") and len(args.image) == 71, "Use a local immutable fixture image ID"


def run(command, env, expected=0):
    done = subprocess.run(command, cwd=ROOT, env=env, capture_output=True, text=True, timeout=180)
    assert done.returncode == expected, (command, done.returncode, done.stdout, done.stderr)
    return json.loads(done.stdout)


def fake_python(root):
    path = root / "fake-python"
    calls = root / "calls.txt"
    path.write_text(f'''#!/usr/bin/env python3
import json,pathlib,sys
calls=pathlib.Path({str(calls)!r})
a=sys.argv[1:]
if '-c' in a:
    with calls.open('a') as f:f.write('probe\\n')
    print(json.dumps({{'version':'5.0.2','package_sha256':'b658165e24eb8c4a27d12586a68593ff27c4f8b3794a41991870b2000f726cc5','files':65,'python':'3.11.fixture'}}))
else:
    with calls.open('a') as f:f.write('grade\\n')
    def arg(name):return a[a.index(name)+1]
    rows=json.loads(pathlib.Path(arg('--predictions_path')).read_text())
    ids=a[a.index('--instance_ids')+1:]
    resolved=[r['instance_id'] for r in rows if r['model_patch'] and '+1\\n' in r['model_patch']]
    unresolved=[r['instance_id'] for r in rows if r['model_patch'] and r['instance_id'] not in resolved]
    empty=[r['instance_id'] for r in rows if not r['model_patch']]
    data={{'total_instances':len(ids),'submitted_instances':len(rows),'completed_instances':len(resolved)+len(unresolved),'resolved_instances':len(resolved),'unresolved_instances':len(unresolved),'empty_patch_instances':len(empty),'error_instances':0,'completed_ids':resolved+unresolved,'incomplete_ids':[],'empty_patch_ids':empty,'submitted_ids':[r['instance_id'] for r in rows],'resolved_ids':resolved,'unresolved_ids':unresolved,'error_ids':[],'schema_version':2}}
    name=rows[0]['model_name_or_path'].replace('/','__')+'.'+arg('--run_id')+'.json'
    pathlib.Path(name).write_text(json.dumps(data))
''')
    path.chmod(0o755)
    return path


temporary = None
if args.output:
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
else:
    temporary = tempfile.TemporaryDirectory(prefix="aicoder-swe-check-")
    output = Path(temporary.name)
try:
    env = {"PATH": os.environ.get("PATH", "")}
    for key in ["RUSTUP_HOME", "CARGO_HOME", "RUSTUP_TOOLCHAIN", "DOCKER_HOST", "DOCKER_CERT_PATH", "DOCKER_TLS_VERIFY"]:
        if key in os.environ:
            env[key] = os.environ[key]
    generation_args = [str(args.example.resolve()), "--output", str(output / "source")]
    if args.image:
        generation_args += ["--image", args.image]
    generated = run(generation_args, env)
    assert generated["summary"]["selected"] == 3 and generated["summary"]["failed"] == 1
    assert generated["summary"]["predictions"] == 3 and generated["summary"]["resumed"] == 3
    assert generated["factory_calls"] == 3 and generated["requests"] == 4
    assert generated["model_network_requests"] == 0 and not generated["grading_data_in_prompt"]
    verified = run([str(args.binary.resolve()), "verify-generation", generated["generation"]], env)
    assert verified["verified"] and verified["summary"]["selected"] == 3
    python = args.official_python.absolute() if args.official_python else fake_python(output)
    grade = [str(args.binary.resolve()), "grade", "--python", str(python),
             "--dataset-name", generated["dataset"], "--predictions", generated["predictions"],
             "--generation-run", generated["generation"], "--run-id", "pipeline_contract",
             "--report-dir", str(output / "grading"), "--timeout", "20", "--total-timeout", "120"]
    scored = run(grade, env)
    assert scored["selected"] == 3 and scored["generation_selected"] == 3
    assert scored["resolved"] == 1 and scored["resolved_rate"] == 1 / 3
    assert scored["generation_failures"] == ["fixture__repo-3"]
    assert scored["official"]["unresolved_instances"] == 1 and scored["official"]["empty_patch_instances"] == 1
    assert scored["scope"] == "all_generation_selected"
    directory = output / "grading/aicoder-swe-grading/pipeline_contract"
    imported = run([str(args.binary.resolve()), "import", str(directory)], env)
    assert imported == scored
    # No Python invocation or Docker launch is needed to reuse validated historical results.
    cached = grade.copy()
    cached[cached.index("--python") + 1] = "/must-not-execute"
    assert run(cached, env) == scored
    if not args.official_python:
        assert (output / "calls.txt").read_text() == "probe\ngrade\n"
    # A changed grading row cannot be joined onto historical generation.
    dataset = Path(generated["dataset"])
    before = dataset.read_bytes()
    changed = json.loads(before)
    changed[0]["test_patch"] = "different hidden fixture"
    dataset.write_text(json.dumps(changed))
    failed = subprocess.run(grade, cwd=ROOT, env=env, capture_output=True, timeout=30)
    assert failed.returncode != 0
    dataset.write_bytes(before)
    result = {"official_harness": bool(args.official_python), "harness_version": scored["harness"]["version"],
              "package_sha256": scored["harness"]["package_sha256"], "selected": 3,
              "correct": 1, "wrong": 1, "generation_failure_placeholder": 1,
              "resolved_rate": scored["resolved_rate"], "generation_calls": generated["factory_calls"],
              "same_input_generation_resume": True, "same_input_grading_cache": True,
              "changed_grading_data_rejected": True, "model_network_requests": 0,
              "classification": "scripted contract fixture; no real SWE-bench/model quality claim"}
    (output / "check-summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result))
finally:
    if temporary:
        temporary.cleanup()
