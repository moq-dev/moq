# [S] A qmux WebSocket record pins no more than the session counts

## Goal

A qmux session over WebSocket counts at least the allocation each received
record actually pins, so a hostile peer cannot hold several times the memory
its flow-control credit and the `Assembler`'s defragmentation are meant to
bound.

## Plan

Found 2026-11-21 while reviewing
[moq-dev/moq#9353](https://github.com/moq-dev/moq/pull/9353). The session
(`rs/moq-quic/src/connection/mux/mod.rs`, `process`) counts each record as
pinning `max(size, MAX_FRAME_SIZE, max_record_size)`, which is 16 KiB by
default. tungstenite slices each unfragmented binary message zero-copy out of
its `in_buffer` (`split_to(len).freeze()` in its frame codec), and that buffer
starts at `WebSocketConfig::read_buffer_size`, 128 KiB by default. qmux leaves
that default in `capped` (`rs/qmux/src/ws.rs`). So a one-byte STREAM record can
pin 128 KiB while it counts as 16 KiB, about 8x per unread chunk on every
stream.

Recommendation: take the stopgap that
[codec](/quest/m9/stream-transports/codec.md) planned for CPU, which also
closes this gap. Set `read_buffer_size` in `capped` to the session's
`max_message_size` (at least `MAX_FRAME_SIZE`), so tungstenite's buffer is
what the session counts. A larger frame grows the buffer with `reserve`, and
Vec's doubling can reach about twice `max_record_size`. Either count that
factor in `process` or keep the grown buffer from backing later records, as
`transport::Stream` does for byte streams. Test with a WebSocket pair: a small
message must not pin more than the session counts.

Measure first with the stream-transport bench
(see the [epic](/quest/m9/stream-transports/README.md)): report WebSocket
throughput, CPU, and retained memory before and after. A smaller read buffer
means more reads per byte.

## Related

- [codec](/quest/m9/stream-transports/codec.md) - replaces tungstenite later; its stopgap moved here
