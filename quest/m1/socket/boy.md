# [S] moq-boy on sockets

## Goal

The moq-boy emulator accepts viewers on a socket endpoint named for its game,
and each viewer dials it from its own endpoint, so neither `rs/moq-boy` nor
`js/moq-boy` watches announcements or requests broadcasts by hand. Each
viewer's commands and latency travel on its own session instead of a viewer
broadcast and the shared `status` map, and crowd play, pausing when nobody
watches, and the 60 s idle stop behave as before.

## Plan

See the [epic](/quest/m1/socket/README.md) for the convention.

- The emulator runs an endpoint named for its game, and each viewer an
  endpoint with its random id that dials it. Settle and record the prefix
  (such as `boy`, so endpoints sit at `boy/.tcp/<id>`).
- The viewer's `command` track at `{viewerPrefix}/{name}/{viewerId}` becomes
  the viewer's messages on its session, one command per group. Drop the
  separate viewer-prefix configuration in `rs/moq-boy/src/main.rs`.
- Per-viewer latency moves from `status`'s `latency` map to the emulator's
  messages on that viewer's session. Game state stays in the ordinary
  `status` broadcast.
- A reload is a new session; the emulator counts a viewer as gone when its
  session ends.
- The demo relay's anonymous grants (`demo/relay/prod.toml`, and the
  `moq auth serve` line it documents) let an anonymous viewer publish its own
  endpoint and subscribe to the game endpoints, instead of boy's use of
  `demo/viewer/**`. Anonymous viewers share one grant, so record how a viewer's
  endpoint id is kept from being claimed by another. Whichever of this and
  [CARLA on sockets](/quest/m1/socket/carla.md) lands second removes
  `demo/viewer/**`.
- Update `doc/bin/demo.md` and the boy README or CHANGELOG where they describe
  the viewer path.

## Required

- [JS sockets](/quest/m1/socket/js.md) - the viewer's dial
