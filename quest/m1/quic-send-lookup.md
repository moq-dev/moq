# [S] Send buffer lookups stay linear

## Goal

A measured verdict on `moq-quic`'s `SendBuffer::get` cost as retained
segments grow, and, if the cost grows with segment count, a lookup whose
per-packet cost does not.

## Plan

Decided 2026-10-22 from the repository improvement audit; m1 because it sits
on every relay's egress path and is cheap to settle before the cut.

`SendBuffer::get` (`rs/moq-quic/src/connection/send_buffer.rs`) walks the
retained segments from the front on every call, and packetization calls it
repeatedly, so sending N retained segments in order visits O(N^2) segments.
The audit's loop replay counted 8,256 visits for 128 segments of 64 bytes and
524,800 for 1,024. With a 1 MB window of ~1 KB writes, each packet walks about
500 segments. Those are visit counts, not measured time. The send window
already bounds retained bytes, so this is CPU only, not memory.

- Measure first: sweep the existing `rs/moq-quic/benches/streams.rs` over
  segment size and count, with ACKs delayed so segments stay retained. Count
  instructions as the [performance audit](/quest/m1/audit-perf.md) does.
- Only if the sweep shows a slope, compare a sequential cursor (the next
  unsent offset's segment index, with a fallback scan for retransmits)
  against indexed segments (offsets searchable by binary search). Keep
  each write as the caller's `Bytes`, with no coalescing copy.
- If the sweep is flat within noise, record that here and delete the quest.

## Related

- [Egress profile](/quest/m2/quic-egress-profile.md) - the broader send-path profile; its `SendBuffer` pool lever shares this type
- [QUIC I/O boundary](/quest/m2/quic-io-boundary.md) - reworks the same transmit buffers
