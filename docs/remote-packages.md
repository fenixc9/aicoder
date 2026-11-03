# Bounded Git and npm resource acquisition

The optional package-downloads feature fetches native resource bundles from Git HTTPS
repositories and npm registries. Both routes finish with the existing allowlisted,
content-addressed PackageStore import. A bundle still declares aicoder-package.json;
the npm route also checks package.json against resolved registry identity. This does
not install a JavaScript runtime, execute lifecycle scripts or activate package tools.

```sh
cargo build --locked -p aicoder-cli --features package-downloads
aicoder packages install-git https://host.example/team/resources.git --revision COMMIT
aicoder packages install-npm @team/resources --version 1.2.3
aicoder packages install-npm @team/resources --registry https://registry.example/ --token-env PACKAGE_TOKEN
```

Git revisions may name a branch/tag/commit; provenance records the resolved full commit.
An omitted npm version resolves latest; tags become concrete versions and explicit versions
must match the response. npm SHA-512 integrity is preferred over SHA-256; a supplied stronger
digest cannot fall back after mismatch. Legacy shasum-only metadata verifies SHA-1 and is
labelled sha1 in provenance; the final package content pin always uses SHA-256. npm package
identity in the downloaded archive must match registry metadata. Different source archives
with identical declared bytes/executable flags produce the same native content pin.

--allow-execution explicitly consents to executable declarations for the imported content;
it never starts them. --disabled imports without global enablement. --timeout sets the
acquisition deadline (default 120 seconds, maximum 600). The default binary rejects remote
commands before constructing storage. Existing local install/inspect commands remain intact.

## Host authority and transport

URLs must use HTTPS, carry no userinfo/query/fragment, and are never automatically redirected.
--allow-loopback-http is an explicit local fixture/self-host policy; it does not admit general
HTTP. npm tarballs must share the host-selected registry origin. No .npmrc, personal Git
configuration, SSH agent, credential helper, Git hook, filter checkout or package lifecycle
script is imported implicitly. Current Git transport is HTTPS, with explicit loopback HTTP
for fixtures; SSH/file/local-Git transports remain outside this acquisition backend.

--token-env is a reference name, bound by the host to the selected origin. Values use the
runtime's layered Environment, not package metadata. HTTP bearer headers stay on that origin;
Git receives a URL-scoped header through its controlled environment, never argv. Values and
server error bodies are excluded from public errors/provenance. SDK hosts can configure
PackageAcquirer::with_environment/with_bearer_reference explicitly (at most eight origins).
Requests do not resolve model credentials and do not create permission rules or restore grants.

The HTTP client makes one attempt per stage without redirects/retries and bypasses ambient
proxy settings. Git uses only fresh host configuration and disables submodules/global config/
helpers. Trusting the source is still necessary: metadata/integrity are not a publisher
signature and the underlying Git/network stack remains part of the host trust boundary.

## Budgets and quarantine

Default bounds are 64 MiB downloaded npm/archive bytes, 128 MiB decoded tar bytes, 16,384
archive headers and extracted nodes, 32 MiB per archive file, 64 path components, 64 KiB
per PAX record and 4 KiB long-path metadata. PAX/GNU metadata is admitted before allocation;
links, devices, FIFOs, sparse files, traversal, duplicate paths, conflicting metadata and
trailing non-padding data reject. Permission import retains only the executable bit. Private
extraction has disk admission and cleanup; only declared native files enter the final object.
The existing package/store quotas apply independently.

Git fetch runs in a fresh Bubblewrap user/PID/mount namespace with read-only system/TLS/DNS
mounts and a dedicated 256 MiB tmpfs capped at 32,768 inodes. Only fixed setup code briefly
holds namespace mount capabilities to apply the size; setpriv removes effective/bounding/
inheritable/ambient capabilities before Git runs. No source checkout occurs: a bare fetch
emits a full commit receipt and a bounded Git tar archive. Native runtime capture owns process
groups, drains pipes, bounds raw artifacts and awaits cleanup; cancellation/drop signals
independently of Tokio, and namespace owner death stops descendants. There is no unbounded
host-clone fallback. Git needs Linux user namespaces, Bubblewrap, mount, setpriv and Git.

The Git disk/time limits bound quarantine storage and active work; the archive limit bounds
captured archive bytes, not an exact network-byte meter. Shared host networking and system
libraries are not a complete network/kernel sandbox. Synchronous filesystem publication uses
the existing bounded package import and cannot preempt a blocked kernel syscall.

## API, persistence and scope

PackageAcquirer::new/for_runtime installs a PackageRequest with explicit limits, options and
TurnExecutionContext. Validation, transfer, integrity and extraction complete before registry
publication. Failures leave the old registry authoritative; completed acquisitions are not
silently retried. Late cancellation is checked again before publication. Existing immutable
objects/orphans retain the normal store admission policy; post-visibility durability errors
require inspection before another attempt.

Registries with only local records stay schema 1. The first remote origin upgrades the private
registry to schema 2; current readers accept both and validate credential-free source metadata.
Old binaries cannot read schema-2 provenance. To downgrade, explicitly import the verified
allowlisted object into a fresh older local store; public package locks/pins and session formats
are unchanged. Do not edit a registry to bypass validation or execution consent.

## Exact dependency-source closures

--source-catalogue FILE maps exact native dependency pins to host-selected Git/npm requests:

```json
{
  "schema_version": 1,
  "entries": [
    {
      "pin": {"id":"shared","version":"1.0.0","sha256":"EXACT_64_HEX_CONTENT_DIGEST"},
      "request": {"kind":"npm","registry":"https://registry.example/","package":"@team/shared","version":"1.0.0"}
    }
  ]
}
```

The catalogue is explicit host input (256 KiB / 32 entries), never project/session discovery.
It cannot define credentials, programs or private storage routes. Missing dependency addresses
must match the complete declared pin; downloaded bytes must reproduce that pin. Existing
trusted content is reused without transfer or trust escalation. Conflicting revisions for
one ID and cycles reject. npm JavaScript dependency/TypeScript ABI resolution is separate from
the native exact dependency graph.

Root plus transitive dependencies have a combined default 32-package limit and separate
256 MiB archive/decoded-byte budgets (metadata remains bounded per request and by count).
The same total acquisition deadline covers the closure. Data stages stay private until the
complete graph validates; only one registry publication selects/trusts the result. Downloads
hold no registry lock. Commit checks the pre-transfer revision under the exclusive lock and
rejects concurrent edits without rolling them back. Errors can retain admitted inert objects,
under the existing store quota, but never partially select/install a dependency graph.

Only the requested root becomes enabled. Dependency active selections are preserved when
already present; a root can use an installed exact older dependency without upgrading the
user's global active revision. Uninstall/revocation/concurrent changes cannot be overwritten
by an old prepared graph. --source-token ORIGIN=ENV can add host-only credential bindings for
dependency origins; the same total eight-origin limit applies. Catalogue metadata cannot mint
these bindings. --token-env remains shorthand for the root origin.

SDK callers use PackageAcquirer::install_with_sources; existing install uses an empty
catalogue and still reuses installed dependencies. Local import keeps its serialized copy/
publication behavior. Session, pin, lock and registry formats are unchanged by catalogue use.

Optional tar/filetime packages stay outside default builds; reqwest, flate2 and base64 reuse
already locked packages. No HTTP/archive dependency enters core. Automated tests use loopback
registry/Git HTTP fixtures and fake credentials; no real model/provider request is needed.

Protocol references: [npm registry version metadata](https://github.com/npm/registry/blob/main/docs/REGISTRY-API.md),
[Git fetch](https://git-scm.com/docs/git-fetch), [Git archive](https://git-scm.com/docs/git-archive).

The [S7 acquisition acceptance](s7-package-download-acceptance.md) and
[atomic closure acceptance](s7-package-graph-acceptance.md) record actual local-service
CLI/runtime checks and the default/optional footprint.
