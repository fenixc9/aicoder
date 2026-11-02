"""A real tiny assertion emitting the status format of the pinned official parser."""
from pathlib import Path

value = Path("/testbed/value.txt").read_text().strip()
print(("PASSED" if value == "1" else "FAILED") + " test_value")
raise SystemExit(0 if value == "1" else 1)
