# [M] Fastest-reader pacing

## Goal

Every hop buffers at most a window of a group past its fastest reader, inside
the group's 32 MB tail, then stops reading the upstream QUIC stream, so stream
flow control pushes back hop by hop to the publisher. A hop with no reader
keeps reading and evicting, so pacing never blocks an unwatched track. A reader
that falls off the tail boards a
[flight](/quest/m1/unbounded-groups/flights.md) when a fill handler exists, and
otherwise refreshes or ends with `Error::Evicted`. A byte stream over MoQ then
loses nothing to a reader that keeps up, and each hop keeps its own congestion
control and retransmissions.

## Plan

Decided 2026-10-25 (maintainer, mesh planning interview), reopening the
2026-10-24 "the live edge is never paced by a reader" decision for all tracks:

- Pace by the fastest reader, never the slowest: one slow reader must not stall
  the publisher or its other readers. Rejected: pacing by the slowest reader
  (TCP fanout semantics), and an opt-in flow-controlled track flag.
- Propagate with QUIC stream flow control per hop. No new wire messages, so it
  lands in the m1 cut beside front eviction
  ([moq-dev/moq#6993](https://github.com/moq-dev/moq/pull/6993)).
  Rejected: explicit MoQ-level credit messages.
- A reader past the window gets no new mechanism: it is an ordinary front
  eviction victim. Tracks without a fill handler, such as
  [byte streams](/quest/m2/stream/README.md), end that reader with
  `Evicted`, which a TCP proxy surfaces as a reset.

Decided 2026-10-28 (maintainer, after an audit of the plan):

- One 32 MB budget per group: the live tail that front eviction
  (`GroupState::trim` in `rs/moq-net/src/model/group.rs`) holds is
  "ahead" (bytes the fastest reader has not read, bounded by pacing) plus
  "behind" (bytes it has read, kept for slower readers). Shedding never
  passes the fastest reader, and the ahead window is at most 16 MB, half
  the tail, so a writer more than the window ahead waits instead of
  shedding unread bytes, and at least 16 MB always stays behind.
  The behind window then slides at the fastest reader's pace with no pacing
  of its own: a reader falls off once it trails the fastest by more than
  32 MB minus the ahead window. The ahead window is each hop's QUIC stream
  receive window, capped at 16 MB. The cap keeps
  [flight](/quest/m1/unbounded-groups/flights.md) admission sound: a reader
  admitted within the behind half is not shed when outstanding credit fills
  the ahead half.
- Reader progress is measured in bytes, not whole frames. Otherwise a frame
  larger than the ahead window deadlocks a writer against a reader that
  advances only on a finished frame. Until
  [byte holes](/quest/m1/unbounded-groups/byte-holes.md) gives Rust a
  chunked read, a whole-frame reader must not stall a paced writer.
- An origin producer that can wait (a byte stream) awaits a readiness future
  that stays pending until a reader exists and the ahead window has room.
  Live encoders ignore it and keep writing. Without the reader condition, a
  dialer writing before its listener subscribes would shed its first bytes
  once they passed 32 MB, and the stream would start with a hole. Relays
  keep the "no reader keeps reading and evicting" rule above, since they only
  hold groups some reader asked for.
- Cap the receive credit paused streams can hold. A paused stream keeps up to
  its stream window counted against the connection window, so enough paused
  groups (one per track with a slow reader) stall unrelated tracks on the
  same connection and trigger moq-quic's receive eviction
  (`CloseKind::Evicted`), which could reset the very byte stream pacing keeps
  lossless. Bring the mechanism to the maintainer before coding, and align it
  with the [receive pool](/quest/m2/receive-pool.md).

Prior art splits the same way (2026-10-28 survey): Varnish streaming, ATS
read-while-writer, and WHATWG `tee()` fan out at the writer's pace and buffer
everything; Varnish `transit_buffer`, nginx unbuffered proxying, and ATS flow
control pace a single client; Node `pipe()` to several destinations paces the
slowest. This is `tee()` with a bounded buffer that drops the branch that
falls off it ([whatwg/streams#1235](https://github.com/whatwg/streams/issues/1235)).

Landed in [moq-dev/moq#7388](https://github.com/moq-dev/moq/pull/7388) (Rust,
session ingest only): readers report the byte end of each frame they take
(`Pace` in `rs/moq-net/src/model/pace.rs`), and the shared transport reader's
payload loop (`coding::Reader::poll_read_frame`) waits on
`group::Producer::poll_ready` before taking more, so moq-lite and
moq-transport ingest both pause past the 16 MB window. A reader that took every
committed frame counts as waiting on the writer, so a frame larger than the
window never stalls it. Tests cover one hop on lite-06, lite-07-wip, and
moq-transport-22: an idle reader holds the relay copy near the window and then
receives every frame, and a slow reader ends `Evicted` while the fast one keeps
up.

What remains:

- The receive-credit cap for paused streams above, which has to land before
  pacing can reset nothing it means to keep lossless. Bring the mechanism to
  the maintainer before coding.
- The readiness future for origin producers: its shape is an API decision to
  bring to the maintainer before coding. Until it lands, a local byte-stream
  writer more than 32 MB ahead of its session's egress sheds its own front.
- JS parity.
- A two-relay chain test with memory measured near the window.
- Benchmarks of the hot path in `group.rs`: the cursor's report is one
  relaxed load per frame or batch, and a store only for the fastest reader.
  They were not run on the busy host that landed the first slice.
- Fills that write a whole batch of groups at once evict their own earlier
  groups before the reader takes them (the follow-up from
  [moq-dev/moq#7339](https://github.com/moq-dev/moq/pull/7339)). Pacing within
  a group does not cover this; decide whether it belongs here or with
  [flights](/quest/m1/unbounded-groups/flights.md).

qmux transports carry the same rule through their own stream credit
([qmux receive lease](/quest/m2/qmux-receive-lease.md)).
