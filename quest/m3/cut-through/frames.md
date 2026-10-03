# [L] Frames fill out of order

## Goal

A `moq-net` frame accepts payload ranges at any offset, a group can hold several
in-flight frames at once, and a reader can consume each range as soon as it
lands. Existing in-order readers and writers behave exactly as before.

## Plan

Decided 2026-09-30:

- `frame::Producer` gains an offset write and `frame::Consumer` gains a read
  that yields `(offset, Bytes)` ranges in arrival order. Both are
  `pub(crate)`: the only users are the lite and IETF sessions in the same crate.
- A frame written out of order keeps the received chunks by offset instead of
  copying into a pre-allocated buffer, so its memory tracks bytes received and
  the per-session pre-allocation `Budget` (`model/frame.rs`) does not apply.
  Each in-flight frame keeps the existing charge-by-bytes-written cache
  accounting (`charge_partial` in `model/group.rs`), per frame. The
  in-order readers (`poll_read_chunk`, `poll_read_all`) still see contiguous
  bytes up to the first hole, and `finish` still requires every byte.
- Overlapping or out-of-bounds ranges are errors.
- A group holds several in-flight frames, not only the one `partial` in
  `GroupState` (`model/group.rs`), and `create_frame_owned` stops rejecting a
  second open frame. Frames are still created in order: frame N+1 exists only
  once N's header, and so its size, is known, because that is what locates
  N+1's header. The group consumer still yields frames in order. Decided
  2026-09-30 after review of #4616, so a hole in frame N does not hold back
  N+1's bytes.
- Today's single sequential writer (`model/frame.rs`, the one `written`
  watermark and its safety comments) is the invariant being changed; keep the
  ordered fast path, including the whole-frame zero-copy install, unchanged.

Extend `rs/moq-net/benches/group.rs` so the ordered path shows no regression,
and test that a frame filled in shuffled ranges reads back identical through
both readers, and that frame N+1's ranges reach a range reader while N still
has a hole.

## Required

- [Bench](/quest/m3/cut-through/bench.md) - a go verdict
