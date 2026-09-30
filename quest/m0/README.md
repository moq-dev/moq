# m0: immediate priorities

## Goal

The work in flight now, in three independent tracks. Relay hardening: legal
moq-transport input never fails a session ahead of Seattle interop on
2026-10-12, every resource a peer can make the relay hold is bounded by what
it sent or by a budget, and no peer input panics the process. Routing: a
service claims the prefix it could serve instead of enumerating broadcasts.
Audio playout: the target is a measured estimate of arrival timing in both
languages, and a browser regression fails a nightly run.

## Plan

The release API gates (#3829..#3878) and the release that followed them are
done. moq.pro tracks this repository as a submodule rather than a release, so
no release quest gates this milestone. The Pronto GPU integration lives in
moq.pro.

Relay hardening: IETF interop leads the ranking, since only those quests
block Seattle. IETF stream types came from m1 in the 2026-09-30 audit
because a session ended by legal input is exactly what Seattle would hit. The DoS hardening
from an external review on 2026-09-29, verified against `main`, stays in m0
as security work. Its quests describe fixes, not exploits.

Routing: the wildcard line is prefix-only on the wire; its resolve and demand
work is done on the line branch and waits to land. Serving the relay's
ingested-only view (`origin::Consumer::local()`) to localhost workers belongs
to moq.pro's edge, which embeds moq-relay; it moved there on 2026-09-28.

Audio playout: the jitter target replaces the round-trip guess. The harness's
browser lane grades it nightly and records the traces it replays; the native
lane is a standalone m1 quest, since nothing here waits on it. The harness
line lands before the jitter line, since both add
`js/watch/src/audio/replay.test.ts`. The [A/V clock](/quest/m1/av-clock.md)
moved to m1 in the 2026-09-30 audit: it waits on the whole jitter line and is
a published `@moq/watch` break on dev.

Published API or wire breaks still land on dev; each quest's Plan says so.

## Required

- [IETF FIN semantics](/quest/m0/ietf-fin-not-cancel.md) - a request stream FIN stops updates without cancelling, and REQUEST_UPDATE on a subscribe is parsed
- [IETF stream types](/quest/m0/ietf-uni-stream-types.md) - padding streams are discarded stream-only and an unknown uni type closes the session, per draft-21
- [Request caps](/quest/m0/request-caps.md) - lite message sizes, IETF request IDs, and per-session announces and subscriptions are bounded
- [quest check everywhere](/quest/m0/quest-check-everywhere.md) - `quest check` guards `main`, `dev`, and the line branches on push and PR, not only PRs into `main`
- [noq reassembly cap](/quest/m0/noq-reassembly-cap.md) - noq carries quinn's stream reassembly cap and the connection receive window is finite by default
- [qmux reset race](/quest/m0/qmux-reset-race.md) - qmux handles RESET_STREAM under one lock instead of panicking
- [Shared fronts](/quest/m0/shared-fronts.md) - viewer sessions share a front, so fronts scale with peers, not viewers
- [Frame alloc budget](/quest/m0/frame-alloc-budget.md) - frame buffers pre-allocate within a per-session budget and otherwise grow with bytes received
- [Handshake deadline](/quest/m0/handshake-deadline.md) - an unfinished handshake or slow HTTP header times out
- [Revalidate overflow](/quest/m0/revalidate-overflow.md) - no auth duration can overflow a deadline and abort the relay
- [Wildcard](/quest/m0/wildcard/README.md) - a relay resolves subscriptions against advertised prefixes, a service claims the prefix it could serve and refuses the rest instead of enumerating broadcasts, and the browser player treats a covering claim as availability
- [Audio quality harness](/quest/m0/audio-quality-harness/README.md) - a browser playout latency regression fails a nightly run instead of arriving as a bug report, and its recorder supplies the jitter target's replay traces
- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the audio playout target is a measured estimate of arrival timing in both languages, not a round-trip guess

## Related

- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/m0/pronto/gpu) - CARLA bridge, release adoption and desktop installation
