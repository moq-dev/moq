# [L] Relay cut-through for group streams

## Goal

The lite and IETF sessions read incoming group streams out of order and write
each payload range to every subscriber's stream at its output offset, so bytes
behind an ingress hole reach the next hop before the hole is filled. On the
[bench](/quest/m3/cut-through/bench.md), cut-through shrinks the post-hole
drain time on the same two-hop path.

## Plan

Decided 2026-09-30: always on when the ingress and egress streams both support
it, no config knob; otherwise today's ordered path.

- Ingest: read the stream header in order, then switch to unordered reads.
  `Reader` may already hold bytes past the header in its buffer, so hand that
  suffix over with its absolute stream offset rather than dropping or
  re-basing it; test a transport chunk that carries both the header and the
  first payload bytes.
  Frame headers are parsed from the contiguous prefix; once a header is known,
  its frame's payload ranges go into the frame with the
  [offset write](/quest/m3/cut-through/frames.md) wherever they land. A hole
  covering the next header blocks later frames, because that header's position
  depends on it. Lite: `FrameIngest` in `lite/subscriber.rs`; IETF:
  `GroupIngest` in `ietf/subscriber.rs`. `Reader::poll_read_frame` in
  `coding/reader.rs` is the ordered path they share today.
- Egress: each subscriber's output offset for a payload range is its stream
  prefix plus every earlier frame's encoded header and size, plus this frame's
  header, plus the range offset. That is known once every earlier input header
  has been parsed, including across a lite and IETF translation, since output
  header lengths depend on values (timestamp and object id deltas). Write the
  header, then the ranges with the
  [offset write](/quest/m3/cut-through/transport.md). Lite: `GroupServe` in
  `lite/publisher.rs`; IETF: `GroupServe` in `ietf/publisher.rs`.
- The fetch paths stay ordered.
- Tests over the mock transport with shuffled chunk delivery: every subscriber
  version receives byte-identical streams to the ordered path, and a
  header-covering hole holds later frames.

Prototype both scopes on the bench: cut-through within the current frame only,
and across in-flight frames, where N+1's payload passes N's hole once N's
header is parsed. Keep the one the numbers justify and delete the other; a
regression test asserts N+1 is forwarded before N's hole fills when the
multi-frame scope ships.

Sweep the bench over publisher and subscriber counts as well as the network
axes. Range bookkeeping, wakeups, and offset writes run per served
subscriber, so total CPU grows with the subscribers a range reaches; the cost
per served subscriber, and the cost from table entries a range does not touch,
must stay flat against the ordered path. Re-run it and record the result in the line's README.

## Required

- [Frame ranges](/quest/m3/cut-through/frames.md) - the model primitive
- [Transport trait](/quest/m3/cut-through/transport.md) - the I/O primitive
