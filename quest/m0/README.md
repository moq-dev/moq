# m0: immediate priorities

## Goal

The work in flight now, in two independent tracks. Relay hardening: legal
moq-transport input never fails a session in current interop coverage, every resource a peer can make the relay hold is bounded by what
it sent or by a budget, and no peer input panics the process. Identity: nothing treats who published a route as what it carries; a path and
the epoch on its route are the only content identity, and every first-party
publisher that can restart mints a fresh epoch, so #4741 stalls nobody.

## Plan

The release API gates (#3829..#3878) and the release that followed them are
done. moq.pro pins this repository's `release` line, so the release gate below
also keeps #4741 from reaching it early.

Per-session request caps landed in #4820, idle fronts in #5054, and the
lost demand-poll wake in #5091. The rest of the 2026-09-29 DoS review remains
in m1 with relay session limits (status refreshed in the 2026-10-10 audit).

Routing: Wildcard landed in #4403, so a service claims the prefix it could
serve instead of enumerating broadcasts. Serving the relay's ingested-only
view (`origin::Consumer::local()`) to localhost workers belongs to moq.pro's
edge, which embeds moq-relay; it moved there on 2026-09-28.

Identity: the [broadcast epoch](/quest/m0/broadcast-epoch/README.md) line
gates the next release (decided 2026-10-03:
#4741 resumes an un-epoched republish into the old broadcast and stalls its
viewers). #4741 can merge to main, but no release ships until first-party
publishers mint epochs. Backport patches cut from `release` don't carry
#4741 and aren't gated (decided 2026-10-08).

Flakes (2026-10-20, maintainer): the redundant failover drift moved here from
the m1 [load round](/quest/m1/test-flakes-2/README.md). It fails loaded
`just check` runs, the local gate every merge waits on, and may hide a TS
export bug.

Hygiene (decided 2026-10-19): the audits sit at the end of this list.
Each recurs once per major release: its PR adds the next run to the following
milestone, gated from m2 onward on the [release cut](/quest/m2/release/cut.md).
Each lands its fixes in its own PR and plans only what needs a decision.

## Required

- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - every first-party publisher that can restart mints a fresh route epoch, the newest wins a path, and only routes with the same epoch resume a subscription
- [Redundant failover drift](/quest/m0/redundant-failover-drift.md) - a stitching TS export switches back to the surviving importer behind bufferbloat instead of failing on a stalled clock
- [qmux 0.5.3](/quest/m0/qmux-0.5.3.md) - crates.io publishes qmux 0.5.3 with the builder session config the release backport of #5609 needs
- [Release shutdown backport](/quest/m0/ffi-shutdown-backport.md) - `release` publishes a non-breaking moq-ffi with an async `drain()` and qmux sessions on the QUIC stream limit
- [Release fixed delay](/quest/m0/release-fixed-delay.md) - a fixed latency on `release`, which moq.dev runs, ignores the catalog's jitter and delay as `main` already does
- [Publisher stall estimate](/quest/m0/publish-stall-estimate.md) - a live browser publish holds its viewers at the Auto target instead of ratcheting their buffer up with every stall
- [CI cost and latency](/quest/m0/ci/README.md) - cloud CI runs only in the merge queue as one `just ci` pass, expensive checks run nightly and gate `release`, caches move to R2, and runners are chosen last
- [Simplification audit](/quest/m0/audit-simplify.md) - overbuilt code and APIs are collapsed, merged, or narrowed into a net simpler whole
- [Lint override and deprecation audit](/quest/m0/audit-lints.md) - every lint suppression is still needed and says why, nothing calls a deprecated API, and stale Rust overrides fail the build
- [Interop audit](/quest/m0/audit-interop.md) - our interop runner entries match what we ship, and every red interop cell with a moq.dev side is fixed, reported, or noted as down
