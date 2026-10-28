# Pi comparison baseline

For S2 and subsequent roadmap work, pin product comparison to **Pi v1.4.0**, release
commit `4a080252f06caf63a525d7873c5d94d3c23887e0`. Verified on 2026-10-29 from the
[official changelog](https://pi.dev/changelog) (release dated 2026-10-21) and the public
[release tag](https://github.com/earendil-works/pi/tree/v1.4.0). The tag was resolved
using `git ls-remote https://github.com/earendil-works/pi.git refs/tags/v1.4.0`.
This pins the comparison target; it does not silently add future upstream changes to scope.

The [official product page](https://pi.dev/) describes tree sessions in one file,
steering after the current tool with remaining tools interrupted, queued follow-ups,
and interactive/print/JSON/RPC/SDK entry points. S2 implements the underlying session,
control and observation contracts needed for those workflows; its event protocol is
our own versioned Rust/JSON API. It does not promise Pi wire/plugin compatibility.

Remaining parity work keeps its existing roadmap placement: shared runtime/project
instructions/tools (S3), semantic long-context management and richer session operations
(S4), complete CLI/TUI/RPC/Web flows (S5), dynamic extensions/MCP/package mechanisms
(S6), and comparable real-task bench/release acceptance (S7). The current checks use
scripted providers; they establish contracts and regressions, not real-model coding
success-rate parity with Pi.
