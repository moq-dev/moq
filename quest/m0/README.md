# m0: immediate priorities

## Goal

The work in flight now. Relay hardening: legal moq-transport input never fails
a session in current interop coverage, every resource a peer can make the
relay hold is bounded by what it sent or by a budget, and no peer input panics
the process. Alongside it: fixes for the `release` line, CI cost, and the
recurring audits.

## Plan

The release API gates (#3829..#3878) and the release that followed them are
done.

Per-session request caps landed in #4820, idle fronts in #5054, and the
lost demand-poll wake in #5091. The rest of the 2026-09-29 DoS review remains
in m1 with relay session limits (status refreshed in the 2026-10-10 audit).

Routing: Wildcard landed in #4403, so a service claims the prefix it could
serve instead of enumerating broadcasts. Serving the relay's ingested-only
view (`origin::Consumer::local()`) to localhost workers belongs to moq.pro's
edge, which embeds moq-relay; it moved there on 2026-09-28.

Hygiene (decided 2026-10-19): the audits sit at the end of this list.
Each recurs once per major release: its PR adds the next run to the following
milestone, gated from m2 onward on the [release cut](/quest/m2/release/cut.md).
Each lands its fixes in its own PR and plans only what needs a decision.

## Required

- [64-bit Hop IDs](/quest/m0/hop-id-64.md) - a moq-transport peer with any non-zero 64-bit Hop ID keeps its session, and Hop IDs are `u64` in Rust and JS
- [MALFORMED_AUTH_TOKEN session code](/quest/m0/malformed-auth-token-code.md) - a 0x16 close reports a malformed token instead of `unknown code=22`, and moq-net sends it for an unparseable token
- [Release shutdown backport](/quest/m0/ffi-shutdown-backport.md) - `release` publishes a non-breaking moq-ffi with an async `drain()` and qmux sessions on the QUIC stream limit
- [HLS admission backport](/quest/m0/hls-admission-backport.md) - `release` refuses the hostile HLS playlists `main` does, without breaking `import::Config::client`
- [Adopt quest resolve](/quest/m0/quest-resolve.md) - local merges resolve concurrent quest link-list edits once kixelated/quest ships its merge driver
- [CI cost and latency](/quest/m0/ci/README.md) - cloud CI runs only in the merge queue as one `just ci` pass, expensive checks run nightly and gate `release`, caches move to R2, and runners are chosen last
