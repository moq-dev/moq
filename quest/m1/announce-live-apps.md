# [M] Apps show "no broadcasts" from the live marker

## Goal

A browser page listing broadcasts shows an empty state once the relay has
said there are none, never a spinner that never resolves and never a false
empty state before the first session answered. The demo watch page and
`@moq/room` use `@moq/net`'s `live` marker, and `@moq/net` settles when an
origin stream opened before the first connection goes live.

## Plan

- #4261 (on `dev`) adds the `live` event, and #4266 the same marker in the
  bindings. Open #4384 renames the announce events to Start/Update/End/Live;
  follow its names if it lands first. The consumers it touches
  (`demo/web/src/index.ts`, `js/room/src/room.ts`, `js/watch/src/broadcast.ts`,
  `js/moq-boy`) skip it today.
- Page load: an origin stream opened before any session connects has no
  session to wait on, so today it goes `live` at once and broadcasts arrive
  after it. Settled: the reconnect loop (`js/net/src/connection/reload.ts`),
  which already answers requests through `expect()`, holds the marker until
  its first session lands `live` or its first dial gives up. An empty list
  then means the relay said so or is unreachable, which a UI can tell apart.
  Once a session is up, its own `live` ends the hold: every wire guarantees
  one (ANNOUNCE_OK, ANNOUNCE_INIT, or the quiet-stream fallback). A peer that
  accepts the announce stream and never answers is a peer bug, so no extra
  timeout.
  Check whether Rust's reconnecting client has the same gap.
- Apps: loading before `live`, an explicit empty state after it with nothing
  announced, and an error state when the connection gives up. Libraries
  expose the state as a signal; wording stays in the demo.
- Tests: an origin stream opened before connect is not `live` until the first
  session is; a connection that cannot connect ends the wait.

Public API: when `@moq/net` emits `live` changes; any state signal on
`@moq/room` or `@moq/watch` is additive. Lands on `dev` with #4261. Wire: none.
