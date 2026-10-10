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
  draft-21 section 7, where datagrams and subgroups share one priority
  order and a datagram wins a tie. Strict datagrams-first lets a busy
  datagram track starve streams up to the congestion window, which is what
  every stack does today.
- **A datagram queue per send group.** Each send group (one per
  subscription, from [the scheduler](/quest/m1/quic/scheduler.md)) owns a
  datagram queue that drains ahead of its own streams, which is the MoQT tie
  rule and puts the subscription's newest data first. Datagram bytes spend
  the group's fair-share credit, so a datagram-heavy subscription buys no
  extra bandwidth over its equal-priority peers. The datagram send API takes
  the same send-group handle as a stream; a datagram without one joins the
  default group. Control streams stay above datagrams because of their
  priority, not because of frame order. Rejected: a separate datagram group
  per subscription (two groups each, and it breaks the tie rule) and a
  global datagram queue compared by scalar (it bypasses the fair tier).
- **Eviction: the lowest-priority oldest datagram goes first.** Keep one
  connection-wide byte budget (`datagram_send_buffer_size`). When a new
  datagram doesn't fit, evict the oldest datagram of the lowest-priority
  group, or drop the new one if it would itself rank lowest. Sending never
  blocks, so moq-net's `poll_send_datagram` loses its Pending case, and
  moq-tokio (drop oldest today) and moq-uring (drop newest today) behave the
  same. Each runtime keeps its current budget size. Rejected: RTT-based
  expiry (a timer path and a tuning constant) and per-group budgets (memory
  grows with subscriptions).
- **Draft:** add a sentence to the Prioritization section of
  `drafts/draft-lcurley-moq-lite.md`: a publisher SHOULD apply a
  subscription's priority to its datagrams as to its streams, and SHOULD
  send the datagram first on a tie. No wire change. Run `just drafts check`.
- **No browser quest**, and no comment on neqo#3813.

Tests in `moq-quic`, reusing the scheduler's saturation fixtures: a
low-priority datagram group waits behind a higher-priority stream, a
high-priority datagram preempts a lower-priority stream, on a tie a group's
datagram goes ahead of its own stream, datagram and stream bytes share one
group's fair-share credit, and eviction removes the lowest-priority oldest
datagram first. In moq-net, a lite and an IETF publisher pass the
subscription's send group with each datagram.

Public API: the datagram send path in `moq-quic`, moq-tokio and moq-net's
transport trait takes a send-group handle, and `poll_send_datagram` no
longer returns Pending. Wire: none.

## Required

- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - supplies
  the send groups and fair tier the datagram queues join

## Related

- [Scope track priority](/quest/m1/track-priority-scope.md) - owns what a
  subscription's priority means; datagrams follow it
- [Datagram replay bound](/quest/m1/datagram-replay-bound.md) - where a new
  datagram subscriber starts in the model's buffer, before the send queue
