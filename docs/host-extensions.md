# Explicit stdio extensions in stock hosts

CLI, TUI and the optional Web host share runtime's immutable extension snapshot. Enable
the optional transports and explicitly select a trusted launch file:

```sh
cargo build --locked -p aicoder-cli -p aicoder-tui --features host-extensions
cargo build --locked -p aicoder-web --features server,host-extensions
target/debug/aicoder --extension-config /absolute/path/launch.json --prompt 'Inspect this project'
target/debug/aicoder-tui --extension-config /absolute/path/launch.json
target/debug/aicoder-web --extension-config /absolute/path/launch.json
```

Existing provider, workspace and approval options still apply. The Web host also requires
its existing host-selected bearer token. Default builds add no transport dependency and
launch no process. A nonempty manifest on a build without the selected transport fails
before launching. Metadata commands do not start children; CLI rejects conflicting flags.

```json
{
  "schema_version": 1,
  "entries": [
    {
      "id": "repository",
      "kind": "mcp_stdio",
      "program": "/absolute/path/to/trusted-server",
      "arguments": ["--root", "{workspace}"],
      "environment_references": {"SERVER_TOKEN": "MY_SERVER_TOKEN"},
      "aliases": {},
      "capabilities": {}
    }
  ]
}
```

The program must be absolute; arguments use literal `{workspace}` substitution, never a
shell. `kind` selects `mcp_stdio` or the native [process protocol](process-extensions.md).
IDs namespace model-facing tools (`repository__name`); aliases/capability keys name remote
tools. All remote tools default to Mutating. A trusted host can explicitly set a capability
to `read_only`, `mutating` or `command`; server annotations cannot grant approval authority.
MCP has no owned prepare/commit contract: approval shows the remote arguments, not an
invented filesystem diff. Native prepared tools retain their exact owned plans.

Environment values are referenced through the runtime Environment (process environment
and an explicitly selected env file). Resolve every reference before the first launch;
missing references fail without starting any entry. Child environment inheritance remains
disabled. Put credential references in the manifest, never credential values in argv or
literal configuration. Unknown fields are rejected without echoing their values.

The manifest is bounded to 256 KiB and 32 entries, with bounded argv, references and catalog
overrides. Existing process/MCP frame, catalog, queue and cleanup limits also apply. It is
host-selected execution authority: project settings, resources, prompts and session history
cannot select or restore it. A trusted executable runs with the user's permissions.

All catalogs are connected, checked for collisions and adopted as one snapshot. A failed
later entry shuts down earlier owners and publishes no partial snapshot. Normal exit,
operation failure and startup cancellation await cleanup; cleanup uncertainty is an error.
Emergency dropped-owner cleanup invalidates retained tools and signals the owned process
groups independently of Tokio. Operations are never implicitly retried after uncertainty.

Settings/resource reload preserves the current registration and connection generation.
Editing the launch file requires restarting the host; it does not replace tools during a
live turn or restore grants. Package executable activation is a separate explicit opt-in
below; packaged theme selection and remote transports remain separate work.

## Rust hosts

`Runtime::activate_extensions(&manifest, context).await` returns an `ActivatedExtensions`
owner. Explicitly adopt `owner.extensions()` with `Runtime::with_extensions`, retain the
owner while using the runtime, then await `owner.shutdown()` before leaving the async
runtime. Runtime builders install that same snapshot; `executor_builder_with_extensions`
combines additional native contributions with collision/budget checks. Cloning a catalog
does not keep a process usable after its owning guard has shut down or dropped.

`scripts/check_host_extensions.py` checks real CLI JSON, RPC, Web HTTP and TUI PTY against
a loopback scripted provider and real MCP child. It verifies normal approval, actual file
effects, child reaping, metadata without launch and cancellation during slow startup.

## Installed package executables

With the same optional host-extensions feature, CLI/RPC/TUI/Web can activate executable
entries from the current installed package snapshot:

```sh
aicoder packages install /trusted/package --allow-execution
aicoder --activate-packages --package-programs /trusted/programs.json --prompt 'Inspect this project'
aicoder-tui --activate-packages --package-programs /trusted/programs.json
aicoder-web --activate-packages --package-programs /trusted/programs.json
```

`--activate-packages` is required even after installation consent. It activates only
host-selected, installed pins with current exact-content execution trust. Selection/locks
and history cannot install or reauthorize code. Bundled executable entries need no program
file; omitted --package-programs supplies an empty registry. Logical program dependencies
resolve only from the explicit host file:

```json
{
  "schema_version": 1,
  "profiles": {
    "node": {
      "program": "/absolute/path/to/node",
      "version": "22.19.0",
      "environment_references": {"SERVER_TOKEN": "MY_SERVER_TOKEN"}
    }
  }
}
```

Names and versions must match the package's exact declared requirement. Versions are host
attestations, not automatic binary-version verification. The file caps at 256 KiB / 64
profiles, with bounded paths and 128 environment references/profile. Only referenced programs
resolve values, after every declaration is validated. Lookup/parser errors do not echo values.
Host program files are never discovered from project settings or session records.

Runtime preflights package trust, dependencies and environment references before starting
direct stdio owners. Direct and package launches share a 32-executable limit and publish one
validated catalog; later failure awaits every started owner. The existing dispatcher still
owns all leaf approvals, guards, budgets and tool effects. Installation and metadata never
start code. These trusted programs use normal user permissions, not the optional comparison
namespaces described in the evaluation docs.

`Runtime::activate_host_extensions(&stdio, Some(&programs), context)` returns the same
ActivatedExtensions guard. The original activate_extensions API remains available. Package
SDK activations also invalidate retained tool clones on drop, without cancelling their host
parent context. Await shutdown for a cleanup report; dropping an owner signals cleanup
independently of Tokio. Package extension catalog versions identify package content with
`package-sha256:<digest>@<peer_stamp>`, retaining the observed peer version/catalog identity;
protocol/catalog freshness checks remain in the adapters. Host program versions remain
attestations rather than automatic binary identity proofs.

Resource reload can select new text resources while retaining the running executable
generation. Its catalog keeps the original package digest; restart explicitly to adopt new
package code. This avoids silently starting a changed revision or restoring backend authority
from public package state. `scripts/check_host_extensions.py --packages` verifies installation
without execution, all four frontends, approvals, startup cancellation and reaping.
See the [stock package acceptance](s7-package-host-acceptance.md) for frozen checks and footprint.
