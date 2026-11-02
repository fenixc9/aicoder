"""Read-only identity probe; -I prevents workspace/PYTHONPATH import substitution."""
import hashlib
import importlib.metadata
import json
import platform
from pathlib import PurePosixPath

distribution = importlib.metadata.distribution("swebench")
names = sorted(str(path).replace("\\", "/") for path in distribution.files
               if PurePosixPath(str(path)).parts[0] == "swebench"
               and "__pycache__" not in str(path))
if not 1 <= len(names) <= 256:
    raise ValueError("Unsupported harness file inventory")
fingerprint = hashlib.sha256()
total = 0
for name in names:
    if ".." in PurePosixPath(name).parts:
        raise ValueError("Invalid harness file path")
    with open(distribution.locate_file(name), "rb") as source:
        content = source.read(4 * 1024 * 1024 + 1)
    total += len(content)
    if len(content) > 4 * 1024 * 1024 or total > 8 * 1024 * 1024:
        raise ValueError("Harness input budget exhausted")
    fingerprint.update(name.encode() + b"\0" + content)
print(json.dumps({"version": distribution.version,
                  "package_sha256": fingerprint.hexdigest(),
                  "files": len(names), "python": platform.python_version()}))
