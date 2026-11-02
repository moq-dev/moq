# [S] moq-boy on sockets

## Goal

The moq-boy emulator accepts viewers through a `Listener` at its game path,
and each viewer dials it, so neither `rs/moq-boy` nor `js/moq-boy` watches
announcements or requests broadcasts by hand. Each viewer's latency comes back
on its own socket instead of the shared `status` map, and crowd play, pausing
when nobody watches, and the 60 s idle stop behave as before.

## Plan

See the [epic](/quest/m2/socket/README.md) for the layout and the reply split.

- Viewers dial `boy/game/<name>` with their random id. Their `command` track
  moves from `{viewerPrefix}/{name}/{viewerId}` to the socket. Drop the
  separate viewer-prefix configuration in `rs/moq-boy/src/main.rs`.
- Per-viewer latency moves from `status`'s `latency` map to a track on that
  viewer's `.accept` broadcast. Game state stays in `status`.
- Requests become pinned to the viewer's epoch (boy requests unpinned today),
  so a reload is a new socket.
- The demo relay's anonymous publish grant (`demo/relay/prod.toml`, and the
  `moq auth serve` line it documents) covers `demo/boy/game/*/.dial/*`
  instead of boy's use of `demo/viewer/**`. Whichever of this and
  [CARLA on sockets](/quest/m2/socket/carla.md) lands second removes
  `demo/viewer/**`.
- Update `doc/bin/demo.md` and the boy README or CHANGELOG where they describe
  the viewer path.

## Required

- [Rust sockets](/quest/m2/socket/rust.md) - the emulator's listener
- [JS sockets](/quest/m2/socket/js.md) - the viewer's dial
