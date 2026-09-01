# [M] One zero-copy stream send per frame

## Goal

Sending one frame over a quiche stream today costs two copying
`stream_send` calls: one for the few header bytes (timestamp delta and size
varints) and one for the payload, each allocating a `RangeBuf` inside quiche
and copying into it, even though the payload is already a refcounted `Bytes`.
Make the egress path hand already-encoded bytes to quiche zero-copy, and let
one egress turn drain a whole prefetched frame batch instead of one frame.

This changes nothing on the wire. The versioned framing happens in
`moq_net::coding::Writer::buffer`, which encodes each version's header bytes
into the internal `BytesMut`; everything below that seam is opaque bytes, so
moq-lite and every IETF draft take the same path.

## Plan

Mechanism, per the 2026-09 survey:

- `Writer::poll_flush` drains the header `BytesMut` and `poll_write` sends
  the payload separately, through `SendStream::poll_write_buf`, whose
  `web-transport-trait` default flattens any `Buf` to `&[u8]` chunks. No
  ownership survives the trait boundary, so quiche's copying
  `stream_send` is the only reachable API.
- quiche 0.29.x exposes `stream_send_zc`, which appends a caller-owned
  buffer to the send queue without copying, behind the `BufFactory`
  generic on `Connection`.

Proposal:

- Configure the quiche `Connection` in `moq-uring` with a `Bytes`-backed
  `BufFactory`.
- Open a zero-copy seam through the transport: an owned-buffer write on the
  send stream (for example a `poll_write_owned(Bytes)` with a copying
  default, coordinated with the `web-transport-trait` crate), so both the
  flushed header bytes (`BytesMut::split().freeze()`) and each payload
  `Bytes` reach quiche as zc appends. Where the seam lands (trait extension
  vs a quiche-backend downcast) is the main open design decision; prefer the
  trait extension so quinn-proto can adopt it later.
- Let the subscribe serve loop push the whole prefetched batch per turn:
  today each frame is header-flush plus payload-write; with owned appends
  the loop can stage N frames before kicking the driver once.
- Respect `Writer`'s ordering contract: the header buffer must always fully
  precede the payload on the stream, including across `Pending`.

Acceptance: count quiche send-queue allocations and copies per frame before
and after, then `just bench BASE` on Linux for relay CPU at the video and
fanout shapes plus throughput ceiling. Latency must not regress. Keep the zc
path ablatable behind the factory type until measured.

## Related

- [#3129](/quest/m1/perf/3129-moq-uring-write-the-webtransport-stream-header-at-open.md) - the WebTransport prefix send on first write, same code area
- [#3201](/quest/m1/perf/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md) - zero-copy one layer down, at the UDP socket
