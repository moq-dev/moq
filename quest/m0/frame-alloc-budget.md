# [M] Frame buffers are backed by bytes received

## Goal

The memory a relay commits to an incoming frame tracks what the peer has
sent, not only the size it declared. Today the first payload byte allocates
the whole declared size (up to `MAX_CACHE_BYTES`, 32 MiB) with
`vec![0u8; size]` and charges it to the cache, with no aggregate bound.

## Plan

- Pre-allocating the declared size is a deliberate optimization, so keep it
  within a per-session budget: a frame whose declared size fits the
  session's remaining budget is allocated up front; past the budget the
  buffer grows as bytes arrive. Bytes are returned to the budget when the
  frame completes or aborts. Decided 2026-09-29.
- Charge the cache by bytes written rather than declared size
  (`model/group.rs` `state.cache += frame.size`).
- Benchmark first (`rs/moq-net/benches`): frame receive throughput swept over
  frame size and concurrent streams, for today, full growth, and the budget,
  so the budget's default comes from the measurement.
- Paths: `model/frame.rs` `MutableFrameBuf::new`, `coding/reader.rs`
  `poll_read_frame`, called from both `lite/subscriber.rs` and
  `ietf/subscriber.rs`.

Public API: none expected; if the budget needs a knob, it goes on the relay
config, not moq-net. Wire: none.

## Related

- [Frame slot charge](/quest/m1/frame-slot-charge.md) - lands first; both change the cache charge in `model/group.rs`
- [Peer limits](/quest/m1/quic/peer-limits.md) - stream counts and windows per peer
