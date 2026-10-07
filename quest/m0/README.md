# m0: immediate priorities

## Goal

The work in flight now, in three independent tracks. Relay hardening: legal
moq-transport input never fails a session ahead of Seattle interop on
2026-10-12, every resource a peer can make the relay hold is bounded by what
it sent or by a budget, and no peer input panics the process. Identity: nothing treats who published a route as what it carries; a path and
the epoch on its route are the only content identity, and every first-party
publisher that can restart mints a fresh epoch, so #4741 stalls nobody.
Audio playout: the target is a measured estimate of arrival timing in both
languages, and a browser regression fails a nightly run.

## Plan

The release API gates (#3829..#3878) and the release that followed them are
done. moq.pro pins this repository's `release` line, so the release gate below
also keeps #4741 from reaching it early.
The Pronto GPU integration lives in moq.pro.

The DoS hardening from an external review on 2026-09-29, verified against `main`, stays in m0
as security work. Its quests describe fixes, not exploits.

Routing: Wildcard landed in #4403, so a service claims the prefix it could
serve instead of enumerating broadcasts. Serving the relay's ingested-only
view (`origin::Consumer::local()`) to localhost workers belongs to moq.pro's
edge, which embeds moq-relay; it moved there on 2026-09-28.

Identity: the [broadcast epoch](/quest/m0/broadcast-epoch/README.md) line
gates the next release (decided 2026-10-03:
#4741 resumes an un-epoched republish into the old broadcast and stalls its
viewers). #4741 can merge to main, but no release ships until first-party
publishers mint epochs.

Audio playout: the jitter target replaces the round-trip guess. The browser
audio quality harness in `test/audio-quality/` has landed; it grades playout
nightly and records the traces it replays. Its native lane is a standalone m1
quest, since nothing here waits on it. The [A/V clock](/quest/m1/av-clock.md)
moved to m1 in the 2026-09-30 audit: it waits on the whole jitter line and is
a published `@moq/watch` break.

## Required

- [Draft-22 LOCATION_FILTER](/quest/m0/ietf-location-filter-22.md) - moqt-22 LOCATION_FILTER carries its type instead of a Length in Rust and JS, so a draft-22 peer reads our Next Object correctly
- [Request caps](/quest/m0/request-caps.md) - lite message sizes, IETF request IDs, and per-session announces and subscriptions are bounded
- [Prefix route fronts](/quest/m0/prefix-route-fronts.md) - a prefix route cannot be made to mint one front per requested path
- [qmux credit](/quest/m0/qmux-credit.md) - qmux returns connection credit for dropped and stopped streams and delivers its close frame, on both lines
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - every first-party publisher that can restart mints a fresh route epoch, the newest wins a path, and only routes with the same epoch resume a subscription
- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the audio playout target is a measured estimate of arrival timing in both languages, not a round-trip guess

## Related

- [Pronto GPU integration](https://github.com/moq-dev/moq.pro/tree/main/quest/m0/pronto/gpu) - CARLA bridge, release adoption and desktop installation
