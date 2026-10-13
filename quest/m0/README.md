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
FIRST_OBJECT at object 0 (#5027); the [release line](/quest/m0/release-22/README.md)
backports both with moq-noq 1.3.5 so Seattle peers get a fixed 0.17.x.

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

Interop on main (2026-10-13): the cross-client matrix fails with `unroutable`
whenever WebSocket wins the connect race: since #5294, #4180's upgrade
retires epochless handles before its held `Restart` reaches anyone. The
released wire-compat step aborts on planned breaks the last release made
stale. Both stay in m0 (maintainer, 2026-10-13): a red Interop hides real
regressions.

Crashes (2026-10-13, #5407): a publisher on a host whose wall clock reads
before 2020 panics, and `panic = "abort"` turns any library panic into a
dead host app. Both joined m0 (maintainer): fix the unset clock at its
source, then unwind so a panic closes only its connection.

Nightly on main (triaged 2026-10-13): two build breaks from 2026-10-11 get
quests here. The nixpkgs bump broke the Linux zigbuild release builds, and
fat LTO in the release profile broke the fuzz build. Audio quality has been
red since it landed, against budgets recorded locally; re-measuring them on
the runner moved here from Watch (maintainer, 2026-10-13). The other red jobs
have fixes on `main`: macOS tests (#5335), drill-sensitivity (#5338), loom
(#5389) and Windows tests (#5339).

## Required

- [CI cost and latency](/quest/m0/ci/README.md) - PRs stop queueing behind hosted runner caps: macOS and Windows move to the merge queue, caches move to R2, and setup is skipped when no work is selected
- [Draft-22 media on 0.17](/quest/m0/release-22/README.md) - a 0.17.x with the LOCATION_FILTER and FIRST_OBJECT fixes and moq-noq 1.3.5, then draft-20+ FETCH
- [Upgrade restarts epochless broadcasts](/quest/m0/upgrade-unroutable.md) - a WebSocket-to-QUIC upgrade retires epochless handles only once their `Restart` is delivered, and first-party consumers follow it, fixing Interop's `unroutable` rounds
- [Unset host clock](/quest/m0/wall-clock-omit.md) - a publisher whose wall clock reads before 2020 publishes without a catalog clock instead of panicking
- [Panic isolation](/quest/m0/panic-isolation.md) - unwind, ignore lock poison, and supervise every task, so a panic closes only its connection or exits on a long-lived task
- [Wire compat on main](/quest/m0/wire-compat-green.md) - the nightly released-wire step runs again: planned breaks re-measured against the new releases, and a released-API client for the released JS lanes
- [Release builds link](/quest/m0/release-link.md) - the Linux zigbuild release binaries and moq-gst packages link again after the 2026-10-11 nixpkgs bump, so the next release cut can publish
- [Fuzz builds under LTO](/quest/m0/fuzz-lto.md) - the nightly fuzz targets build again despite the release profile's fat LTO
- [Audio quality budgets](/quest/m0/audio-quality-budgets.md) - the nightly Audio quality job grades against ceilings measured on its own runner, so a breach means a regression again
- [web-transport releases the qmux fixes](/quest/m0/qmux-credit-upstream.md) - waiting on moq-dev/web-transport#412 and #413 to merge and ship, which qmux credit bumps to
- [qmux credit](/quest/m0/qmux-credit.md) - qmux returns connection credit for dropped and stopped streams and delivers its close frame, on both lines
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - every first-party publisher that can restart mints a fresh route epoch, the newest wins a path, and only routes with the same epoch resume a subscription
