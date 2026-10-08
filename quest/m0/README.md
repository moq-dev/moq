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

Relay hardening left in m0 is idle fronts: per-session request caps landed
in #4820, and the rest of the 2026-09-29 DoS review moved to m1 with
relay session limits.

Routing: Wildcard landed in #4403, so a service claims the prefix it could
serve instead of enumerating broadcasts. Serving the relay's ingested-only
view (`origin::Consumer::local()`) to localhost workers belongs to moq.pro's
edge, which embeds moq-relay; it moved there on 2026-09-28.
Pools of claim workers (transcoders) also need the relay to forget a front
nobody reads (found 2026-10-07).
A demand poll that loses a reader's wake keeps an unread front and its
upstream subscription alive (found 2026-10-08).

Interop: Fastly's moq-relay-interop report (run of 2026-09-23, build
7ee2b02) was triaged against `main` on 2026-10-07. Its SETUP, UNSUBSCRIBE
and error-code items were already fixed. Fastly's reruns that day found
that a relay moves End of Track's Location and refuses imquic's End of
Group status. The fixes below LOCATION_FILTER come from them and go ahead
of Seattle. The cold-relay Largest stays compliant with INVALID_RANGE
(decided 2026-10-08), and the deviations doc landed in #5022.
An imquic draft-22 rig (2026-10-08) hit LOCATION_FILTER and a clear
FIRST_OBJECT at object 0 (#5027); the [release line](/quest/m0/release-22/README.md)
backports both with moq-noq 1.3.4 so Seattle peers get a fixed 0.17.x.

Identity: the [broadcast epoch](/quest/m0/broadcast-epoch/README.md) line
gates the next release (decided 2026-10-03:
#4741 resumes an un-epoched republish into the old broadcast and stalls its
viewers). #4741 can merge to main, but no release ships until first-party
publishers mint epochs. Backport patches cut from `release` don't carry
#4741 and aren't gated (decided 2026-10-08).

Liveness: a serve loop with work always ready never yields, which starved
an FFI publisher's QUIC driver and fails hosted Interop's go lanes (found
2026-10-08 landing #4225). The [serve budget](/quest/m0/serve-budget.md)
bounds every kio task's loop. The Go and Python interop cells it fails
moved here from m1 the same day, because the stall masks interop on every
wire PR and hides as slow passes; the harness now fails a cell whose
connection idles out.

Audio playout: the jitter target is default in both languages (#4162); the
[line](/quest/m1/audio-jitter-target/README.md) moved to m1 in the
2026-10-08 audit, since only a manual browser proof and a native trace replay
remain and no release waits on them.

## Required

- [Serve budget](/quest/m0/serve-budget.md) - a kio task that always has work ready yields after a budget, so a fast publisher can't starve its own QUIC driver
- [Draft-22 LOCATION_FILTER](/quest/m0/ietf-location-filter-22.md) - moqt-22 LOCATION_FILTER carries its type instead of a Length in Rust and JS, so a draft-22 peer reads our Next Object correctly
- [Draft-22 media on 0.17](/quest/m0/release-22/README.md) - a 0.17.x with the LOCATION_FILTER and FIRST_OBJECT fixes and moq-noq 1.3.4, before Seattle
- [Capped stream END_OF_GROUP](/quest/m0/ietf-end-of-track-location.md) - a stream capped by the subscription's end Location never claims END_OF_GROUP; moving End of Track's Location is deferred
- [End of Group status](/quest/m0/ietf-end-of-group-status.md) - an End of Group status on a stream whose header already marks the group's end is accepted, so imquic's last object per group arrives
- [Demand lost wake](/quest/m0/demand-lost-wake.md) - a reader that comes and goes between a demand poll and its re-read never leaves a front or `broadcast::Demand` without a wake
- [FFI publisher stall](/quest/m0/ffi-publisher-stall.md) - every Go and Python publisher cell passes reliably once the serve budget lands, and a cell fails when a connection idles out
- [web-transport releases the qmux fixes](/quest/m0/qmux-credit-upstream.md) - waiting on moq-dev/web-transport#412 and #413 to merge and ship, which qmux credit bumps to
- [qmux credit](/quest/m0/qmux-credit.md) - qmux returns connection credit for dropped and stopped streams and delivers its close frame, on both lines
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - every first-party publisher that can restart mints a fresh route epoch, the newest wins a path, and only routes with the same epoch resume a subscription
- [moq-noq 2.0.2 on main](/quest/m0/noq-2.0.2.md) - `main` carries the max datagram size fix `release` gets in 1.3.4
