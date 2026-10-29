# [M] MKV import streams blocks

## Goal

The MKV importer streams a SimpleBlock or BlockGroup payload into its frame
as bytes arrive instead of buffering the whole tag, so a tag declaring
gigabytes holds memory only as fast as the output accepts it.

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

Remaining work:

- Settle how non-seekable input handles BlockGroup information that arrives
  after the payload. Recommended: explicitly refuse the cases whose output
  depends on those trailing fields, unless a seekable-input path can inspect
  the metadata first. This changes accepted MKV behavior and needs a scope
  decision. Supporting every existing input instead requires a staging design
  with explicit storage ownership and bounds; simply buffering the block in
  another layer does not fix this quest. Metadata child handling is decided.
- Implement incremental metadata state and a framing reader that streams
  blocks and discards ignored payloads while preserving EBML hierarchy,
  parent boundaries, unknown-size Segment/Cluster handling, and EOF errors.
- Connect block chunks to the container producer without bypassing grouping,
  timestamp normalization, estimation, catalog, HLS, or selected output framing.
  Cover lacing, skipped tracks, and subtitle transformation explicitly.
- Feed huge declared blocks, metadata masters, and ignored elements in chunks;
  assert retained input stays bounded and output becomes readable before a
  block completes. Cover trailing BlockGroup fields and every split of the
  variable-length headers. Extend the existing `mkv-chunks` benchmark.

No importer implementation is included in the current draft. Reproduce the
buffering with a chunked, huge declared element before changing the importer.

Public API: none expected. Wire: none.
