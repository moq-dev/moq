# [M] Receive leases keep exact books

## Goal

moq-quic's receive leases and connection window stay exact under every
sequence an application or peer can drive: no lease entry outlives its
stream, eviction ranks streams by credit they actually hold, shrinking then
growing the receive window credits the peer exactly once, and evicting under
a stuck window costs O(log n) instead of a scan of every stream.

## Plan

Found in the 2026-10-23 moq-quic audit (`connection/streams/lease.rs`,
`streams/state.rs`). Reproduce each with a test first.

- **Entries for dead streams.** `set_priority` and `fill` call
  `leases.entry(id).or_default()` without checking that `recv` still holds
  the stream. `set_priority` followed by a plain read to FIN leaves an empty
  `Leased` for the connection's lifetime (the driver's `Drop` skips `stop`),
  and a lease after `stop` recreates the entry, so a later `stop` returns
  `Ok` instead of `ClosedStream`. Keeping `Leased` inside the stream's
  receive state removes the second map, its scan loop, and the vacant-arm
  special cases in `stop`.
- **Reset streams ranked by returned credit.** After `RESET_STREAM`,
  `stuck_victims` still counts `end - bytes_read` as held, though `reset()`
  already returned it, so evicting the stream frees nothing and its reader
  sees `Evicted` instead of `Reset(code)`. Rank by
  `unread_before(reliable_size)` in the reset state, plus any bytes a lease
  has taken but not yet released, which still hold credit.
- **Window shrink debt.** `set_receive_window` adds the full growth to
  `local_max_data` without first cancelling `receive_window_shrink_debt`:
  window 100, shrink to 50, grow to 100 gives `local_max_data` 150. Subtract
  `min(growth, debt)` from the debt first.
- **Eviction cost.** `stuck_victims` walks every receive stream and lease and
  allocates per eviction, and a peer can force one eviction per round trip
  by spending each freed credit on another incomplete message. Keep an
  ordered set keyed by rank, updated as streams change state. Add a bench
  swept over stream count, so a cost that grows with the table shows up as a
  slope.
- **Lost transmit flag.** `lease()` drops `fill()`'s `ShouldTransmit` on its
  `TooLarge` and `Reset` returns, delaying `MAX_STREAM_DATA` and
  `MAX_STREAMS` to the next wake. Propagate it on every return.

Tests: repeated eviction cycles; a reset (including `RESET_STREAM_AT` below
the leased offset) while a lease holds taken bytes; eviction, `stop`, then a
late FIN sending exactly one `STOP_SENDING` and one `MAX_STREAMS`; shrink
then grow; a `TooLarge` or `Reset` lease that freed credit sends
`MAX_STREAM_DATA` and `MAX_STREAMS` without waiting for another wake. Extend the connection fuzz target past `lease(0)` to real `want`
values, asserting `held` equals the sum of the leases' held bytes.

## Related

- [qmux receive leases](/quest/m2/qmux-receive-lease.md) - brings leases to qmux on the same state machine
