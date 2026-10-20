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

Interop: Fastly's moq-relay-interop report (run of 2026-09-23, build
7ee2b02) was triaged against `main` on 2026-10-07. Its SETUP, UNSUBSCRIBE
and error-code items were already fixed. Fastly's reruns that day found
that a relay moves End of Track's Location and refuses imquic's End of
Group status. The fixes below LOCATION_FILTER come from them and go ahead
of Seattle. The cold-relay Largest stays compliant with INVALID_RANGE
(decided 2026-10-08), and the deviations doc landed in #5022.
An imquic draft-22 rig (2026-10-08) hit LOCATION_FILTER and a clear
FIRST_OBJECT at object 0 (#5027). Both shipped with moq-noq 1.3.5 in
moq-relay 0.17.3 and `@moq/net` 0.4.3; [release notices](/quest/m0/release-22/README.md)
remain for the testers and reporters of the later clock and quiet-edge fixes.

Identity: the [broadcast epoch](/quest/m0/broadcast-epoch/README.md) line
gates the next release (decided 2026-10-03:
#4741 resumes an un-epoched republish into the old broadcast and stalls its
viewers). #4741 can merge to main, but no release ships until first-party
publishers mint epochs. Backport patches cut from `release` don't carry
#4741 and aren't gated (decided 2026-10-08).

Liveness: a serve loop with work always ready never yields, which starved
an FFI publisher's QUIC driver and fails hosted Interop's go lanes (found
2026-10-08 landing #4225). The publish serve loops now yield through a
`kio::coop::Budget`. The Go and Python interop cells it fails
moved here from m1 the same day, because the stall masks interop on every
wire PR and hides as slow passes; the harness now fails a cell whose
connection idles out.

Audio playout: the jitter target is default in both languages (#4162); the
[line](/quest/m1/audio-jitter-target/README.md) moved to m1 in the
2026-10-08 audit, since only a manual browser proof and a native trace replay
remain and the release only requires the production clock and playback contracts
that depend on them.

Interop on main (2026-10-13): the cross-client matrix failed with `unroutable`
whenever WebSocket won the connect race: since #5294, #4180's upgrade
retired epochless handles before its held `Restart` reached anyone. Handles
now retire only once it is deliverable. First-party consumers following it
moved to m1 (maintainer, 2026-10-13), since the upgrade is removed.

Crashes (2026-10-13, #5407): a publisher on a host whose wall clock reads
before 2020 panics, and `panic = "abort"` turned any library panic into a
dead host app. Both joined m0 (maintainer) and are fixed: the clock at its
source, and unwinding so a panic closes only its connection.

Nightly on main (triaged 2026-10-13): two build breaks from 2026-10-11 get
quests here. The nixpkgs bump broke the Linux zigbuild release builds (its
GCC 16 miscompiles lld 21, fixed upstream and waiting on `nixpkgs-unstable`),
and fat LTO in the release profile broke the fuzz build. Audio quality has been
red since it landed, against budgets recorded locally; re-measuring them on
the runner moved here from Watch (maintainer, 2026-10-13). The other red jobs
have fixes on `main`: macOS tests (#5335), drill-sensitivity (#5338), loom
(#5389) and Windows tests (#5339).

Hygiene (decided 2026-10-19): ten audits sit at the end of this list.
Each recurs once per major release: its PR adds the next run to the following
milestone, gated from m2 onward on the [release cut](/quest/m2/release/cut.md).
Each lands its fixes in its own PR and plans only what needs a decision.

## Required

- [qmux 0.5 upstream](/quest/m0/qmux-credit-backport-upstream.md) - old-release publication, independent of the main fix

- [qmux release backport](/quest/m0/qmux-credit-backport.md) - adopt the 0.5 fix independently of the next breaking cut

- [Discarded-Retry backport](/quest/m0/retry-backport.md) - one injected or corrupted Retry no longer kills a handshake on the release line, via a moq-noq 1.3.x patch

- [CI cost and latency](/quest/m0/ci/README.md) - PRs stop queueing behind hosted runner caps: macOS and Windows move to the merge queue, caches move to R2, and setup is skipped when no work is selected
- [Draft-22 release notices](/quest/m0/release-22/README.md) - tell testers and reporters which published releases contain their fixes
- [nixpkgs ships the lld fix](/quest/m0/nixpkgs-lld-fix.md) - waiting on NixOS/nixpkgs#568680 to reach `nixpkgs-unstable`, which the release build fix bumps to
- [Release builds link](/quest/m0/release-link.md) - the Linux zigbuild release binaries and moq-gst packages link again after the 2026-10-11 nixpkgs bump, so the next release cut can publish
- [web-transport releases the qmux fixes](/quest/m0/qmux-credit-upstream.md) - waiting on #412 to publish the patched 0.6.x that main adopts
- [qmux credit](/quest/m0/qmux-credit.md) - main adopts qmux 0.6 credit and close fixes; the 0.5 backport is independent
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - every first-party publisher that can restart mints a fresh route epoch, the newest wins a path, and only routes with the same epoch resume a subscription
- [Performance audit](/quest/m0/audit-perf.md) - benchmarks that regressed since the last run are explained, fixed, or planned, and unbenched hot paths get coverage
- [Dead code and API surface audit](/quest/m0/audit-dead-code.md) - unused code, exports, features, and flags are deleted or planned for deletion
- [Compile time audit](/quest/m0/audit-compile-time.md) - build-time hotspots are measured against a baseline and the worst are fixed or planned
- [Test and CI cost audit](/quest/m0/audit-test-cost.md) - the slowest, flakiest, and least valuable tests and CI jobs are fixed, deleted, or planned
- [Test coverage audit](/quest/m0/audit-test-coverage.md) - untested behavior that matters is covered or planned, and every test runs in CI
- [Documentation audit](/quest/m0/audit-docs.md) - stale pages, examples, links, and undocumented exports are fixed or planned
- [Dependency and alternatives audit](/quest/m0/audit-deps.md) - dependencies are current, deduplicated, needed, and the best maintained option
- [Artifact size audit](/quest/m0/audit-size.md) - binary, FFI, WASM, and bundle growth since the last run is explained, fixed, or planned
- [Simplification audit](/quest/m0/audit-simplify.md) - overbuilt code and APIs are collapsed, merged, or narrowed into a net simpler whole
- [Quest tree audit](/quest/m0/audit-quests.md) - the tree agrees with itself and with main
