# [M] Schedule datagrams by subscription priority

## Goal

In `moq-quic`, a queued QUIC datagram is scheduled by its subscription's
priority in the same hierarchy as streams. A low-priority datagram track no
longer preempts higher-priority streams, and a high-priority one still goes
ahead of lower-priority streams. Both the lite and IETF publishers in
`rs/moq-net` hand each datagram to its subscription's send group.

Today every quinn-derived stack (moq-noq, noq, `moq-quic`) packs every
queued DATAGRAM frame before any STREAM frame in `populate_packet`
(`rs/moq-quic/src/connection/mod.rs`, the `// DATAGRAM` loop ahead of
`// STREAM`), so datagrams beat every stream, control streams included, and
are FIFO among themselves. Nothing in moq-net orders them either.

Out of scope: browsers. Chrome's QUICHE also sends datagrams first, and its
datagram `sendOrder` (`createWritable`) only orders datagrams among
themselves behind an experimental flag. Firefox's neqo starves datagrams
behind normal streams ([neqo#3813](https://github.com/mozilla/neqo/issues/3813)).
Neither can express a unified order, so js/net is unchanged.

## Plan

Decided 2026-10-10 with the maintainer:

- **Unified priority, not "datagrams always first".** It matches MoQT
  draft-21 section 5.1.2, where datagrams and subgroups share one priority
  order and a datagram wins a tie within one group. Strict datagrams-first lets a busy
  datagram track starve streams up to the congestion window, which is what
  every stack does today.
- **A datagram queue per send group.** Each send group (one per
  subscription, from [the scheduler](/quest/m1/quic/scheduler.md)) owns a
  datagram queue. Datagram bytes spend
  the group's fair-share credit, so a datagram-heavy subscription buys no
  extra bandwidth over its equal-priority peers. The datagram send API takes
  the same send-group handle as a stream; a datagram without one joins the
  default group.
- **A datagram carries its order within the group**, the same value a stream
  gets (its group sequence), so it merges into the subscription's
  newest-first order: a queued datagram for group 10 waits behind a stream
  for group 11. Only on a tie, a datagram and a stream of the same group,
  does the datagram go first, which is MoQT draft-21's rule (group order
  before forwarding preference). Decided 2026-10-10 after review: draining
  every datagram ahead of the group's streams broke newest-first. Control
  streams stay above datagrams because of their priority, not because of
  frame order. Rejected: a separate datagram group
  per subscription (two groups each, and it breaks the tie rule) and a
  global datagram queue compared by scalar (it bypasses the fair tier).
- **Eviction is domain-aware: the lowest-priority oldest datagram goes
  first.** Keep one connection-wide byte budget
  (`datagram_send_buffer_size`). When a new datagram doesn't fit, pick the
  scheduling domain holding the most queued datagram bytes (on a tie, the
  domain whose oldest queued datagram is oldest), then evict the oldest
  datagram of that domain's lowest-priority group, oldest first among equal
  priorities. Counting bytes, not datagrams, keeps eviction byte-fair, the
  same as the send tier. Drop the new datagram instead if it would itself be that
  victim. Priorities are compared only inside a domain, because
  [Scope track priority](/quest/m1/track-priority-scope.md) confines them to
  the broadcast when cluster fairness is on; with fairness off there is one
  domain. Decided 2026-10-10 after review: a connection-wide comparison let
  one busy high-priority broadcast evict every datagram of another. Sending
  never blocks, so the poll-shaped `poll_send_datagram` becomes a
  synchronous `send_datagram` that can't represent Pending, and moq-tokio
  (drop oldest today) and moq-uring (drop newest today) behave the same. Each runtime keeps its current budget size. Rejected: RTT-based
  expiry (a timer path and a tuning constant) and per-group budgets (memory
  grows with subscriptions).
- **Draft:** add a sentence to the Prioritization section of
  `drafts/draft-lcurley-moq-lite.md`: a publisher SHOULD apply a
  subscription's priority and group order to its datagrams as to its
  streams, and SHOULD send the datagram first when both carry the same
  group. No wire change. Run `just drafts check`.
- **No browser quest**, and no comment on neqo#3813.

Tests in `moq-quic`, reusing the scheduler's saturation fixtures: a
low-priority datagram group waits behind a higher-priority stream, a
high-priority datagram preempts a lower-priority stream, a datagram for an
older group waits behind a stream for a newer one, a datagram goes ahead of
a stream of its own group, datagram and stream bytes share one group's
fair-share credit, eviction removes the lowest-priority oldest datagram
first, and with fairness on two saturated broadcasts both keep sending
datagrams, including when one sends many small datagrams and the other few
large ones. In moq-net, a lite and an IETF publisher pass the subscription's
send group and group sequence with each datagram.

Eviction fans out over domains, send groups, and queued datagrams, so add a
benchmark sweeping each axis independently (many broadcasts with one group
each included). Picking a victim must not scan every domain or group on each
enqueue: keep a byte counter per domain in a heap keyed by queued bytes, and
a per-domain heap of groups keyed by priority.

Public API: the datagram send path in `moq-quic` and moq-net's transport
trait (`poll_send_datagram` in `rs/moq-net/src/transport.rs`) becomes a
synchronous `send_datagram` taking a send-group handle and a group
sequence.
Every implementation follows: moq-tokio (`transport.rs`,
`transport/owned.rs`), moq-uring (`transport/adapter.rs`,
`quic/noq/connection.rs`, `quic/web.rs`), moq-ffi and moq-wasm
(`transport/adapter.rs`), moq-net's `ietf/adapter.rs`, and the test mocks.
moq-uring's `quic/web.rs` serves WebTransport over the worker's own QUIC
connection, so it honors the handle and order like raw QUIC. moq-wasm sits
on the browser's WebTransport, which cannot rank a datagram against
a stream, so it ignores the handle and order and keeps its current
best-effort send. Wire: none.

## Required

- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - supplies
  the send groups and fair tier the datagram queues join

## Related

- [Scope track priority](/quest/m1/track-priority-scope.md) - owns what a
  subscription's priority means; datagrams follow it
- [Datagram replay bound](/quest/m1/datagram-replay-bound.md) - where a new
  datagram subscriber starts in the model's buffer, before the send queue
