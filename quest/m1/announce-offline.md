# [XL] Announce streams toggle between live and offline

## Goal

An announcement stream in `@moq/net` and `rs/moq-net` reports when it stops
being live, not only when it starts. `live` fires once a connection's replay
has landed; a new `offline` event fires when no connection feeding the stream
is up. Events strictly alternate from an implicit not-live start: `live`,
`offline`, `live`, and so on.

- Every connection path follows the same rule (JS `Reload`, one-shot
  `connect`/`accept`, and reconnects), so page load is no special case: a
  stream opened before any connection starts not-live, and a failed attempt
  never lands, so it never yields `live`.
- On `offline` the stream emits no `end`: the set is held as it was. When the
  next replay lands, the stream emits only the real diffs (`end` what's gone,
  `start` what's new, `update` the rest), then `live`. A reconnect blip tears
  nothing down in a player.
- JS delivers through one pending entry per prefix, like Rust's `pending`
  map, so both sides share the per-prefix `live` barrier and the same
  coalescing.

## Plan

Decided by the maintainer (2026-09-29):

- Toggle, with no special case for the first connection. Why: treating the
  first connection as special is weird; a UI needs a marker for when things
  stop being live (reconnecting), not only for when they are.
- `offline` is an event in the announce stream, not app state read from
  `Reload.status`. Why: the stream is what knows whether its set is current.
- Never report `live` until there's actually a live connection: a failed
  one-shot `connect`/`accept` leaves the stream not-live. Why: a false `live`
  on an empty set makes a player conclude "offline" too early.
- The initial state is implicit: no `offline` at open. Why: a stream starts
  not-live, so the first event is either a route or `live`.
- Hold and reconcile: on `offline`, emit no `end`; reconcile when the next
  replay lands. Why: a blip must not tear down every player.
- Rust mirrors the same shape in `OriginConsumer`: `offline` when the last
  network source feeding the consumer's scope goes away, `live` when a new
  one's replay lands. A local-only origin with no connections is live at
  once, as today. Why: one model across Rust, JS, and the bindings.
- Rewrite the JS queue now instead of waiting for generated lite. Why: the
  per-prefix pending map gives the reconcile, and Rust's per-prefix barrier
  comes with it, so JS matches Rust today.

Implementation:

- Sources. Rust's `Producer::replaying` guard (`rs/moq-net/src/model/origin.rs`)
  lives only while a session replays. Widen it into a per-session source that
  lives for the whole session and marks when its replay landed; the lite and
  IETF subscribers (`rs/moq-net/src/{lite,ietf}/subscriber.rs`) hold it. A
  cursor is live once every connected source overlapping its scope has landed
  (the per-prefix barrier: `LiveState::Owed` as today), and offline once none
  is connected. JS's `replaying` signal (`js/net/src/origin.ts`) widens the
  same way. `Reload` (`js/net/src/connection/reload.ts`) registers its source
  before its first dial and keeps it across reconnects; one-shot
  `Connection.connect`/`accept` register theirs before the handshake and
  drop it on failure.
- Local-only: an origin no connection has fed is live at once. Once a
  connection has been attached, only a landed replay makes a stream live, so
  a failed one-shot never releases into `live`.
- Reconcile. While offline, a cursor stops delivering and keeps folding
  changes per prefix. Across the gap, a prefix that ends and comes back is
  compared by route (hops, cost), not by the session serving it: unchanged
  delivers nothing, changed delivers `update`. Rust's pending map folds
  `Unannounce` then `Announce` into an `UnannounceAnnounce` today; it needs
  this offline fold. Prefixes still pending when the replay lands are owed
  ahead of `live`.
- JS queue. Replace the append-only `AnnounceState.queue`
  (`js/net/src/announced.ts`) and the per-tick table diff in
  `#runAnnounced` with one pending entry per prefix, taken in path order,
  folding and cancelling as Rust's `apply_announce`/`apply_unannounce` do.
- Open, ask the maintainer before building: whether the hold applies to
  session egress cursors (`rs/moq-net/src/{lite,ietf}/publisher.rs`). Held
  retractions there keep advertising a dead route to peers during the gap,
  which delays failover. Recommendation: egress forwards retractions at once
  and only app-facing cursors hold.
- Bindings: moq-ffi's announce event (`rs/moq-ffi/src/origin.rs`) gains
  `Offline`; follow the moq-ffi row of the cross-package sync table.
- Docs, updated with the code: `doc/concept/moq-lite.md`,
  `doc/lib/js/net.md`, `doc/lib/rs/moq-net.md`, and the binding pages that
  describe the `live` marker.
- Benchmark: the pending map grows with both prefixes and announcement
  consumers. Add a `js/net/bench` case swept over prefix count and consumer
  count, with slow consumers draining it, and wire it into
  `.github/workflows/nightly.yml` next to the other JS origin benchmarks.
  Add an offline-then-reconnect case over the same sweep to
  `rs/moq-net/benches/origin.rs`.
- Tests, in Rust and JS: a stream opened before the first connection gets
  `live` only after its replay; a failed first connection (one-shot and
  `Reload`) yields no `live`; a disconnect yields `offline` with no `end`; a
  reconnect with the same set yields only `live`, and with a changed set the
  real diffs then `live`; a second connection dropping while another is up
  yields nothing; barrier cases for a change to an owed prefix (folded ahead
  of `live`), a retraction of one (cancelled, `live` still follows), and a
  new prefix sorting ahead of an owed one (may precede `live`).

Public API: breaks `@moq/net`'s announce `Event` type and `rs/moq-net`'s
announce consumer (`AnnounceEvent`), and the moq-ffi bindings, so it lands on
`dev`. Wire: none; `offline` is local connection state, and the drafts carry
only the initial-set count (`Active Count`), no live marker.
