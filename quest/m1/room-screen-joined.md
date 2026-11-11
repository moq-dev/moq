# [S] A screen share publishes only while joined

## Goal

`@moq/room`'s `Local` publishes nothing, screen share included, while the
user has not joined a room or after they leave. Nobody sees a screen the user
did not share into the room.

## Plan

Found 2026-11-10 by the room chart ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`js/room/verify/Room.tla` breaks `LeavingStopsPublishing` in 1 step. In
`js/room/src/local.ts` the screen broadcast is enabled by `#screenLive` alone,
not gated on `enabled`, so sharing before joining, or leaving while sharing,
publishes the screen as a screen-only participant with no name.
`residual: a screen share publishes while not joined and after leaving` in
`js/room/src/local.test.ts` pins it.

Gate the screen broadcast on `enabled && #screenLive`, fix `Local`'s doc,
which describes today's behaviour, flip the residual test, and update the
Room chart so its rule holds. Check `moq-room` (Rust) for the same gate.

Caused by: [moq-dev/moq#3634](https://github.com/moq-dev/moq/pull/3634), which
extracted conferencing into `@moq/room` with this gate.

Public API: none (behaviour fix). Wire: none.
