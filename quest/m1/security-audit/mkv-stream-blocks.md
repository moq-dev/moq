# [M] MKV import streams blocks

## Goal

The MKV importer streams supported block payloads into their frames as bytes
arrive instead of buffering whole tags. Ignored bytes are discarded as they
arrive. Cases that cannot produce correct output in one pass are explicitly
refused before the affected payload is published.

## Plan

From the 2026-10-27 audit (MUX-3). `rs/moq-mux/src/container/mkv/import.rs`
never sets ebml-iterable's tag limit, whose default is about 4 GB, and
SimpleBlock and BlockGroup are buffered whole.

The audit's fix (lower `max_allowable_tag_size`) is wrong: the library applies
it to every sized tag, including the Segment and Cluster masters, so it would
refuse ordinary files larger than the limit.

Decided 2026-10-27 (maintainer): stream the block payload, the same principle
as [fMP4 import streams mdat](/quest/m1/security-audit/fmp4-stream-mdat.md),
rather than a size pre-check. A pre-check would compare against the frame cap
that [unbounded groups](/quest/m1/unbounded-groups/README.md) removes, after
which a large block is a legal large frame. This needs a small block reader
beside ebml-iterable for the block header and payload; prefer an upstream
ebml-iterable option if one fits. Ignored elements are discarded as their
bytes arrive instead of buffered.

Decided 2026-10-28 (maintainer, implementation brief): handle metadata
children incrementally. `Info`, `Tracks`, and `TrackEntry` must not retain
all children until the declared master ends. Keep only the active track's
consumed configuration fields; discard ignored children as bytes arrive.
Do not introduce a blanket tag-size cap.

Investigation 2026-10-28: ebml-iterable 0.7.1 exposes incremental master
start/end events, but binary leaves still require the entire payload and
there is no public skip/stream hook. Removing masters from its capture list
alone does not stream `SimpleBlock`, `Block`, or ignored binary elements.
`container::Producer::write` also takes a complete `Frame`, although
moq-net's underlying frame producer accepts chunks.

A remaining design constraint is `BlockGroup`: `ReferenceBlock` may follow
`Block`, and its absence decides whether the block opens a keyframe group.
Subtitle `BlockDuration`, `BlockAdditions`, and `CodecState` may also follow
the payload. The importer currently consumes that information at master end.
Streaming the payload into a published frame before reading those children
cannot preserve the existing grouping and subtitle conversion semantics.
See [Matroska block semantics](https://www.matroska.org/technical/elements.html#ReferenceBlock).

Decided 2026-10-29 (maintainer implementation brief, with recommended code
choices authorized): adopt the draft's recommended explicit refusal of
non-seekable cases whose output depends on trailing BlockGroup fields. This
is no longer an open maintainer decision. Do not introduce an unbounded
staging store or a new public API to preserve those cases.

There is no seekable-input path to reuse: `Import::decode` accepts pushed byte
slices, and `Import::seek(sequence)` changes output group numbering, not the
input position. The current API therefore remains a one-pass importer.

The one-pass support contract to implement, checked before payload publication:

- Audio/video SimpleBlock payloads stream with their existing grouping and
  timestamp semantics. Parse Xiph, EBML, and fixed-size lace headers
  incrementally, retaining at most their 256 frame lengths. Laced tracks still
  require a positive DefaultDuration, with all timestamps checked before the
  first frame is published.
- An audio BlockGroup can stream because the importer does not use its trailing
  ReferenceBlock, BlockDuration, additions, or CodecState. A video BlockGroup
  can stream when a preceding ReferenceBlock establishes delta-frame grouping,
  or when Block ends exactly at its sized parent's end, making the absence of
  a trailing reference knowable. Refuse other video BlockGroups before opening
  their frame, including a keyframe Block followed by apparently harmless
  children: another ReferenceBlock could still follow those children.
- Unknown/skipped-track payloads are discarded incrementally and do not need
  output grouping decisions.
- Refuse subtitle tracks at TrackEntry completion, before advertising their
  rendition. SRT escaping and WebVTT timestamp rebasing can change output
  length based on payload contents, whereas the output frame needs its length
  up front. Moving the subtitle body into another buffer is not a fix. Trailing
  duration, additions, and CodecState are further reasons the existing whole
  cue conversion cannot be published early.
- Keep Legacy and LOC output framing. Refuse selected output paths that cannot
  emit their prefix from known metadata before publishing a rendition, rather
  than silently selecting a different container. The current CMAF writer takes
  complete Frames; its streaming capability still needs to be evaluated.

Checkpoint 2026-10-29: the importer implementation is still unchanged. A focused
**desired-behavior reproduction test** is added to `import.rs`, declaring a
1 GiB EBML master with a nearly 1 GiB Void and feeding 1 KiB chunks. The test
asserts that ignored bytes do not accumulate and truncated EOF is refused.
It is expected to fail against the current implementation, but has not run:
`quest gate just rs test -p moq-mux --lib -E
'test(ignored_payload_does_not_accumulate)'` remained queued for host capacity.
The queued command was cancelled at handoff. `quest check` (664 documents),
`git diff --check`, and focused rustfmt passed. `just ci` encountered the
unchanged `sh/rs/test-lints.test.py` failure: mbx could not execute Cargo
because of `Argument list too long (os error 7)`. Its selected Rust workload
also stayed queued at the gate; the remaining CI processes were cancelled.
The reproduction has neither compiled nor executed, and no memory or
backpressure measurement was made. This is an unfinished draft, not a
memory/backpressure security fix.

Remaining work:

- Run the reproduction before changing production code. Replace complete-leaf
  decoding with an incremental framing reader, preserving hierarchy, parent
  boundaries, unknown-size Segment/Cluster handling, and truncated EOF errors.
  Parse metadata children incrementally, retaining only consumed configuration
  fields rather than arbitrary children or their repeated history.
- Implement the support/refusal contract above, with typed errors before
  affected output is published. Evaluate CMAF at setup; support it through
  existing framing facilities only if they can stream correctly, otherwise
  refuse it explicitly. Update user-facing MKV guidance for the narrowed input
  support and mark the implementation PR title as breaking.
- Connect accepted block chunks through the container producer, preserving
  grouping, timestamp normalization, estimates, catalog, HLS, and selected
  output framing. `container::Producer::write` currently requires a complete
  Frame. Merely calling the lower-level net frame writer would bypass those
  responsibilities; it also provides no importer polling/backpressure API.
  Establish the actual output storage ownership/bounds, including cancellation
  of an unfinished frame, instead of claiming parser scratch alone fixes memory.
- Validate huge declared blocks, metadata masters, and ignored payloads in
  chunks; verify retained input and output allocation, output readability
  before block completion, and behavior when output stalls or aborts. Cover
  skipped tracks, every split of variable-length headers, lacing, allowed and
  refused BlockGroup layouts, subtitle setup refusal, and selected containers.
  Extend and measure `mkv-chunks` across payload and transport chunk sizes.
- Run focused tests, `just ci`, and the required gateway/cross-language interop
  checks. No production fix or benchmark measurement is validated yet. Keep
  this work independently reviewable from the concurrent fMP4 quest.

Public API and wire impact of this checkpoint: none. The implementation will
narrow accepted MKV input; it must document those refusals and migration to
one-pass layouts explicitly. No seekable-input API or staging layer is planned.
