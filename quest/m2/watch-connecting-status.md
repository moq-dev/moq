# [M] A watcher reads connecting, not offline, while its connection is down

## Goal

`@moq/watch` says a broadcast is offline only when its relay connection is up
and the broadcast is not announced. While the player dials, redials, or was
refused, its status and UI say so instead, and a refused token shows why.

## Plan

Found 2026-11-10 by the watch and failover charts ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`js/watch/verify/Watch.tla` breaks `OfflineMeansOffline` in 0 steps and
`RefusalIsExplained`, and `Failover.tla` breaks `OfflineOnlyWhenUnreachable`
in 1. `Broadcast.status` in `js/watch/src/broadcast.ts` reads `"offline"`
whenever the player holds no broadcast handle, so OFFLINE flashes on every
page load and after any drop of the viewer's own connection, and the badge
that could say CONNECTING or DISCONNECTED (`live-badge.ts`) is hidden exactly
then. Viewers read their own network blip as "the stream ended". The two
`residual:` tests in `js/watch/src/offline.test.ts` pin it.

Decided 2026-11-10 (maintainer, /quest-plan): ✅ `Broadcast.status` gains a
value for "the connection is not up" (name it by role, such as
`"connecting"`), and the UI shows it and `connection.error` for a refused
token (rejected: a UI-only fix that keeps `"offline"` in the API, so API
consumers keep misreading a dead connection). This changes `@moq/watch`'s
status union, so it is breaking and joins m2; its Impact section says how to
migrate. Mirror the shape in any binding that reports watch status.

Flip the residual tests, update the Watch and Failover charts so their rules
hold, and refresh `js/watch/verify/README.md`.

Caused by: no single PR; status has conflated "no handle" with "offline"
since the watch element read status from the broadcast handle.

Public API: breaking, `@moq/watch` status. Wire: none.
