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

Decided 2026-10-20 (maintainer): [moq-net follows announced
instances](/quest/m0/net-announced-follow.md) moved here from m1, because
catalog references builds `Source::follow` on it instead of hand-rolling
another follow loop; m1's [sink restart follow](/quest/m1/sink-restart-follow.md)
reuses that helper.

Release builds: the 2026-10-11 nixpkgs bump broke the Linux zigbuild release
builds ([release link](/quest/m0/release-link.md)). Decided 2026-10-20
(audit): it and the epoch line rank above the qmux 0.5 backport, because both
gate the next cut while the backport only patches the shipping line.

Hygiene (decided 2026-10-19): the audits sit at the end of this list.
Each recurs once per major release: its PR adds the next run to the following
milestone, gated from m2 onward on the [release cut](/quest/m2/release/cut.md).
Each lands its fixes in its own PR and plans only what needs a decision.

## Required

- [moq-net follows announced instances](/quest/m0/net-announced-follow.md) - the Rust equivalent of JS `request(path, { announced: true })`, replacing the follow loops in moq-ffi and moq-c; catalog references builds `Source::follow` on it
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - every first-party publisher that can restart mints a fresh route epoch, the newest wins a path, and only routes with the same epoch resume a subscription
- [Release builds link](/quest/m0/release-link.md) - the Linux zigbuild release binaries and moq-gst packages link again after the 2026-10-11 nixpkgs bump, so the next release cut can publish
- [qmux 0.5 upstream](/quest/m0/qmux-credit-backport-upstream.md) - old-release publication, independent of the main fix
- [qmux release backport](/quest/m0/qmux-credit-backport.md) - adopt the 0.5 fix independently of the next breaking cut
- [CI cost and latency](/quest/m0/ci/README.md) - cloud CI runs only in the merge queue as one `just ci` pass, expensive checks run nightly and gate `release`, caches move to R2, and runners are chosen last
- [web-transport releases the qmux fixes](/quest/m0/qmux-credit-upstream.md) - waiting on #412 to publish the patched 0.6.x that main adopts
- [qmux credit](/quest/m0/qmux-credit.md) - main adopts qmux 0.6 credit and close fixes; the 0.5 backport is independent
- [Performance audit](/quest/m0/audit-perf.md) - benchmarks that regressed since the last run are explained, fixed, or planned, and unbenched hot paths get coverage
- [Compile time audit](/quest/m0/audit-compile-time.md) - build-time hotspots are measured against a baseline and the worst are fixed or planned
- [Artifact size audit](/quest/m0/audit-size.md) - binary, FFI, WASM, and bundle growth since the last run is explained, fixed, or planned
- [Simplification audit](/quest/m0/audit-simplify.md) - overbuilt code and APIs are collapsed, merged, or narrowed into a net simpler whole
- [Quest tree audit](/quest/m0/audit-quests.md) - the tree agrees with itself and with main
