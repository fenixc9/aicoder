"""Remove only containers of this fresh, host-created harness invocation."""
import json
import sys
import time
import docker

# Called only after the Rust owner stops its own fresh process, never from stored history.
expected = json.loads(sys.argv[1])
removed, errors = [], []
client = docker.from_env(timeout=10)


def owned():
    result = []
    for container in client.containers.list(all=True):
        name = container.name
        if any(name == prefix or
               (name.startswith(prefix + ".") and name[len(prefix) + 1:].isdigit())
               for prefix in expected):
            result.append(container)
    return result


quiet_since = None
deadline = time.monotonic() + 20
while time.monotonic() < deadline:
    containers = owned()
    if containers:
        quiet_since = None
    else:
        quiet_since = quiet_since or time.monotonic()
        if time.monotonic() - quiet_since >= 2:
            break
    for container in containers:
        try:
            identifier = container.id
            client.containers.get(identifier).remove(force=True)
            removed.append(identifier)
        except Exception as error:
            errors.append(type(error).__name__)
    time.sleep(0.25)
remaining = [container.id for container in owned()]
print(json.dumps({"removed": removed, "remaining": remaining, "errors": errors,
                  "observation": "owned names absent for a bounded two-second quiet window"}))
