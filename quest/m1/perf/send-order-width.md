# [L] Widen the transport send order so groups stop needing a queue

## Goal

`web_transport_trait::SendStream::set_priority` takes a `u8`, and that single
signature is why the Lite publisher carries a `PriorityQueue` at all. The queue
exists to compress an unbounded total order over live group streams,
`(track desc, subscribe asc, group desc)`, into 256 dense ranks, which costs a
session-wide lock, a vec shift, and a wake per reordered group.

Widen the send order and most of that disappears for the backends that can
carry it: the rank becomes a value each group computes for itself.

## Plan

What each backend takes natively today:

- quinn: `i32`, narrowed by the trait and widened straight back.
- browser: `sendOrder`, `long long` in the W3C spec; `web-transport-wasm` takes
  an `i32`.
- quiche: `stream_priority(id, urgency: u8, incremental)`, lower urgency first.
  A real cap, not an artifact.
- qmux: `u8`, into its own bucketed scheduler.

So the split is quinn and browsers on one side, quiche and qmux on the other.
Both sides are defaults somewhere: quiche for moq-uring, quinn for moq-tokio.

An exact order-preserving pack of the full key needs 136 bits, so this only
works because exactness is only required among *live* streams:

- `group` needs no structure at all. "Newest first" is "most recently opened
  first", so a per-subscription counter bumped at group open already ranks them.
- `subscribe` cannot be truncated, because a session-old subscription and a
  fresh one span an arbitrary id gap. It needs a rank among live subscriptions,
  but that table is per-subscription rather than per-group, so it changes orders
  of magnitude less often than the queue does today.

An `i64` layout of `[track: 8][subscribe_rank: 16][group_ordinal: 39]` makes the
per-group path a counter bump: no lock, no shift, no wake, and `GroupServe`
loses its `poll_next` priority arm.

Open question for the narrow backends. quiche's 256 urgency levels fit `track`
exactly, and `incremental: true` would round-robin within a track, which is what
`lite::priority`'s standing round-robin TODO wants. But it drops "newest group
first", which is what makes a congested track shed its backlog instead of its
live edge. Decide that before assuming quiche can drop the queue too; the
fallback is keeping the queue on the narrow backends only, at the cost of two
scheduling paths to hold consistent.

Entry cost is a breaking `web-transport-trait` release, rippling through
web-transport-quinn, web-transport-wasm, qmux, iroh, and moq-net's own
`transport::poll::SendStream`.

Acceptance: the wide backends open and close a group without touching a shared
lock, the existing send-order and ordering tests pass unmodified, and `just
bench BASE` shows no relay regression on the narrow path.

## Related

- [Priority set_track wakes](/quest/m1/perf/priority-set-track-wakes.md) - dead if the queue goes away
