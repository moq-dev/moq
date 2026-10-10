# m0: immediate priorities

## Goal

The work in flight now, in two independent tracks. Relay hardening: legal
moq-transport input never fails a session ahead of Seattle interop on
2026-10-12, every resource a peer can make the relay hold is bounded by what
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
remain and no release waits on them.

## Required

- [Self-hosted CI](/quest/m0/self-hosted-ci.md) - same-repo Check and Test run on a self-hosted NixOS runner with a main-written local cache, behind a `CI_RUNNER` kill switch
- [CI host](/quest/m0/ci-host.md) - the maintainer brings up the spare desktop as the `moq-ci` and `moq-gpu` runner host
- [Draft-22 media on 0.17](/quest/m0/release-22/README.md) - a 0.17.x with the LOCATION_FILTER and FIRST_OBJECT fixes and moq-noq 1.3.5, before Seattle
- [TS duration fidelity](/quest/m0/ts-duration-fidelity.md) - Interop's TS compliance captures the whole round-tripped stream again, fixing a regression that turns Interop red on main
- [Paused spinner](/quest/m0/watch-paused-spinner.md) - the watch buffering spinner never covers the paused play button, so Interop's resume step can click it
- [Check base](/quest/m0/check-base.md) - `just check` diffs against the PR's base whatever the local branch tracks, so scoped checks stay scoped
- [web-transport releases the qmux fixes](/quest/m0/qmux-credit-upstream.md) - waiting on moq-dev/web-transport#412 and #413 to merge and ship, which qmux credit bumps to
- [qmux credit](/quest/m0/qmux-credit.md) - qmux returns connection credit for dropped and stopped streams and delivers its close frame, on both lines
- [JS track takeover](/quest/m0/js-track-takeover.md) - JS `createTrack` answers a queued request and continues its sequences, as Rust does, so a re-announced `@moq/publish` catalog never restarts its groups
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - every first-party publisher that can restart mints a fresh route epoch, the newest wins a path, and only routes with the same epoch resume a subscription
