# Migrating @moq/boy

How to update your code for each breaking change, newest first.

## Unreleased

### Viewers dial the game's socket endpoint

Each `<moq-boy>` runs one `socket.Endpoint` with a random `viewer-` id under the game prefix and dials each game it plays, instead of publishing a broadcast under a viewer prefix. It needs the `moq-boy` emulator from the same release.

- Drop the `prefix-viewer` attribute and the `prefixViewer` property.
- `GameConfig` takes `endpoint` in place of `viewerPrefix`.
- `Game.viewerId` is gone; read this viewer's latency from `Game.latency`.
- `GameStatus.latency` is gone; `GameStatus.players` counts the open viewer sessions.
- The element offers only `moq-lite-07-wip`, which sockets need, so the relay must list it in `listen.version`.
