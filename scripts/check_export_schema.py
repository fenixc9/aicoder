#!/usr/bin/env python3
"""Development-only canonical export schema check; local schema references only."""
import json
from pathlib import Path
import sys
import jsonschema
from referencing import Registry, Resource
ROOT = Path(__file__).resolve().parents[1]
schema = json.loads((ROOT / "docs/protocol/session-export-v1.schema.json").read_text())
events = json.loads((ROOT / "docs/protocol/events-v1.schema.json").read_text())
jsonschema.Draft202012Validator.check_schema(schema)
registry = Registry().with_resource(events["$id"], Resource.from_contents(events))
validator = jsonschema.Draft202012Validator(schema, registry=registry, format_checker=jsonschema.FormatChecker())
paths = [Path(p) for p in sys.argv[1:]] or [ROOT / "docs/protocol/session-export-v1.json"]
for path in paths:
    value = json.loads(path.read_text())
    validator.validate(value)
    if len(value["entries"]) != value["manifest"]["entry_count"]:
        raise SystemExit(f"{path}: incomplete canonical entry count")
    print(f"Validated {len(value['entries'])} canonical entries in {path.name}")
