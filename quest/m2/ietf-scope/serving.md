# [M] moq-transport FETCH serving in JS and track listing are gone

## Goal

js/net no longer answers moq-transport FETCH as a publisher, and moq-net no
longer lists a moq-transport peer's tracks with SUBSCRIBE_TRACKS
(`ietf/subscribe_tracks.rs`, `ietf/subscriber/tracks.rs`, and the JS
`subscribe_tracks.ts`). A received SUBSCRIBE_TRACKS is refused, as it is
today.

## Plan

Part of the [IETF scope freeze](/quest/m2/ietf-scope/README.md), decided
2026-11-09. JS FETCH serving and its pacing landed in
[moq-dev/moq#6945](https://github.com/moq-dev/moq/pull/6945),
[moq-dev/moq#7793](https://github.com/moq-dev/moq/pull/7793),
[moq-dev/moq#7794](https://github.com/moq-dev/moq/pull/7794),
[moq-dev/moq#8117](https://github.com/moq-dev/moq/pull/8117), and
[moq-dev/moq#8231](https://github.com/moq-dev/moq/pull/8231); a browser
publisher serving a third-party FETCH has no user. Relays keep answering
FETCH from their cache, and a subscriber keeps its joining FETCH.

Before deleting track listing, confirm no relay or moq.pro path needs it to
bridge a moq-transport publisher into moq-lite announcements; if one does,
keep that part, record why here, and delete the rest. A JS publisher that
receives a moq-transport FETCH answers it with the error moq-transport
defines for an unsupported request.

[Subscribe ranges](/quest/m2/subscribe-ranges/README.md) fills relay misses
over moq-transport; this quest leaves relay FETCH alone.

Public API: breaking only if js/net exposes a FETCH-serving option or Rust a
track-listing call publicly; name each in the PR's Impact section. Wire:
none; both are optional moq-transport behaviors.
