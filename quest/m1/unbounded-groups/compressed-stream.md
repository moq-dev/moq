# [M] Compressed frames have no size cap and inflate as a bounded stream

## Goal

A compressed frame of any size publishes and is received in Rust and
`@moq/net`, like a plain one. The 32 MiB guard on a compressed frame goes.
The wire still declares each compressed slice's size (and its `Inflated
Size`) up front, so a publisher still compresses a frame whole before it
sends it. A receiver never inflates a frame whole in memory: it inflates as
a stream into the frame's segments, which shed under the group's 32 MB tail
like any frame. Inflating past the declared `Inflated Size` fails the frame
as malformed the moment the output passes it, so a zip bomb costs at most one
segment beyond what was declared. A compressed frame near any size no longer
encodes into a slice that receivers refuse.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411).

- [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411) kept a 32 MiB whole-frame guard on compressed frames in JS, reasoning
  that lifting it needs a wire change. The user: "remove the 32MB guard, make
  sure we stream any decompression to avoid zip bombs." And: "we still need
  compressed size on the wire, so we don't need to uncompress to learn it."
  So no wire change: keep the declared compressed size, drop only the guard,
  and bound decompression while streaming.
- Milestone: ✅ m1. Not chosen: m2.
- JS (after [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411)): drop the guard and the `Encoder.bound` preflight in
  `js/net/src/group.ts` (`writeFrame`, `createFrame`, binding compression)
  and JSON's mirrored `MAX_FRAME_BYTES`. Inflate a received slice in chunks
  into the chunked frame storage [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411) added, checking output against the
  declared `Inflated Size`.
- Rust: the guard is `rs/moq-net/src/model/group/content.rs:117` (`size` and
  `inflated` against `MAX_CACHE_BYTES`). The inflated copy inflates each
  frame whole (`GroupState::inflate`, `rs/moq-net/src/model/group.rs:619`).
  Inflate into `frame::SEGMENT` segments instead, bounded the same way.
- This also resolves [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411)'s Rust follow-up: `GroupProducer::write_frame` and
  `write_frames` (`group.rs:1683`, `:1762`) check only the plain payload
  against 32 MB, so an incompressible frame near the cap encodes to a slice
  every receiver refuses. With no compressed cap, there is nothing to
  mismatch; [Byte holes](/quest/m1/unbounded-groups/byte-holes.md) removes
  the plain cap.
- The lite draft's FRAME text ("a receiver MUST check it against its limits
  before inflating") stays; add that a receiver may inflate incrementally and
  must stop at the declared size. Update `doc/concept` and the compression
  docs that state the 32 MiB cap.
- Tests, both languages: a compressed frame several times 32 MB round-trips
  over lite-07 and a relay chain with memory held near the tail; a slice
  whose payload inflates past its declared size fails as malformed without
  allocating the excess.

Public API: removes the compressed-frame `FrameTooLarge` refusal (JS, Rust,
JSON preflight). Wire: none.

## Required

- [JS unbounded groups](/quest/m1/unbounded-groups/js.md) - [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411) lands JS chunked frame storage, which streamed inflation writes into
- [Byte holes](/quest/m1/unbounded-groups/byte-holes.md) - Rust frames past 32 MB at all
