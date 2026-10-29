#!/usr/bin/env python3
"""Development-only JSON Schema check; never required by the agent runtime."""
import json
from pathlib import Path
import sys

import jsonschema

ROOT = Path(__file__).resolve().parents[1]
schema = json.loads((ROOT / "docs/protocol/events-v1.schema.json").read_text())
jsonschema.Draft202012Validator.check_schema(schema)
validator = jsonschema.Draft202012Validator(schema, format_checker=jsonschema.FormatChecker())
paths = [Path(path) for path in sys.argv[1:]] or [ROOT / "docs/protocol/events-v1-trace.jsonl", ROOT / "docs/protocol/approval-preview-v1-trace.jsonl", ROOT / "docs/protocol/session-approvals-v1-trace.jsonl", ROOT / "docs/protocol/model-context-v1-trace.jsonl", ROOT / "docs/protocol/summary-context-v1-trace.jsonl"]
count = 0
for path in paths:
    for line_number, line in enumerate(path.read_text().splitlines(), 1):
        try:
            validator.validate(json.loads(line))
        except (ValueError, jsonschema.ValidationError) as error:
            raise SystemExit(f"{path}:{line_number}: {error}") from error
        count += 1
print(f"Validated {count} event frames against schema v1")
