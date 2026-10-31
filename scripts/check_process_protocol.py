#!/usr/bin/env python3
"""Development-only validation of the synthetic process-tools wire fixture."""
import json
from pathlib import Path
import jsonschema

ROOT = Path(__file__).resolve().parents[1]
schema = json.loads((ROOT / 'docs/protocol/process-tools-v1.schema.json').read_text())
jsonschema.Draft202012Validator.check_schema(schema)
validator = jsonschema.Draft202012Validator(schema)
pending = set()
count = 0
for line in (ROOT / 'docs/protocol/process-tools-v1.jsonl').read_text().splitlines():
    frame = json.loads(line)
    validator.validate(frame)
    if 'method' in frame:
        assert frame['id'] not in pending
        pending.add(frame['id'])
    else:
        assert frame['id'] in pending
        pending.remove(frame['id'])
    count += 1
assert not pending
print(f'Validated {count} synthetic process-tool frames against aicoder.tools.v1')
