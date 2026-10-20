# [L] Segmented frames

## Goal

A large frame is stored in moq-net as a chain of sealed segments instead of
one contiguous buffer. Receiving it allocates at most one segment ahead, not
its full declared size. QUIC receive credit is released as each segment is
handed to the cache. Frames below the segment size keep today's single-`Bytes`
storage and cost nothing extra. Nothing user-visible changes, and the caps
stay for now.

## Plan

See the [epic](/quest/m2/unbounded-groups/README.md) for the decisions.

- Today a completed frame is one `Bytes`, and a declared-size frame
  preallocates up to the session `Budget` (`model/frame.rs`). Readers already
  stream an in-flight frame by byte offset. Keep that cursor, and let it walk
  across segment boundaries.
- Segments are the unit that [Byte holes](/quest/m2/unbounded-groups/byte-holes.md)
  later evicts and refills, so give them a stable byte range within the frame.
- Choose the segment size from benchmarks, and keep it a storage detail
  rather than a public knob. Coalescing the chunks the transport delivers is
  fine.
- Update [Receive memory](/quest/m1/quic/receive-memory.md) so a frame's
  payload releases credit per segment. The frame header stays a whole unit.
- Measure `group.rs`, `frame.rs` (`frame_recv`), and `memory.rs` before and
  after. Add a large-frame case swept over frame size.

Public API: none. Wire: none.
