#!/usr/bin/env python3
"""Development-only RPC frame/schema verification, with a local core schema registry."""
import json
from pathlib import Path
import sys

import jsonschema
from referencing import Registry, Resource

ROOT = Path(__file__).resolve().parents[1]
schema = json.loads((ROOT / "docs/protocol/rpc-v1.schema.json").read_text())
events = json.loads((ROOT / "docs/protocol/events-v1.schema.json").read_text())
jsonschema.Draft202012Validator.check_schema(schema)
registry = Registry().with_resource(events["$id"], Resource.from_contents(events))
validator = jsonschema.Draft202012Validator(
    schema, registry=registry, format_checker=jsonschema.FormatChecker())
paths = [Path(path) for path in sys.argv[1:]] or [ROOT / "docs/protocol/rpc-v1-trace.jsonl"]
count = 0
for path in paths:
    for number, line in enumerate(path.read_text().splitlines(), 1):
        try:
            validator.validate(json.loads(line))
        except (ValueError, jsonschema.ValidationError) as error:
            raise SystemExit(f"{path}:{number}: {error}") from error
        count += 1
request={"version":1,"id":100,"command":{"type":"cancel_session_selection","request_id":99}}
validator.validate(request);count+=1
for invalid in (0,9007199254740992):
    request["command"]["request_id"]=invalid
    assert list(validator.iter_errors(request)), "unsafe session cancellation ID accepted"
for point in ({"position":"current"},{"position":"before_last_user"},{"position":"before","entry":"user-entry"},{"position":"at","entry":"entry"}):
    request={"version":1,"id":101,"command":{"type":"fork_session","point":point}};validator.validate(request);count+=1
request["command"]["point"]={"position":"before"}
assert list(validator.iter_errors(request)), "fork-before accepted a missing source entry"
print(f"Validated {count} RPC frames against schema v1")
