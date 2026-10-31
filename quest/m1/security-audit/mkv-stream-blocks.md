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
as the fMP4 importer's streamed mdat ([moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232)),
rather than a size pre-check. A pre-check would compare against the frame cap
that [unbounded groups](/quest/m1/unbounded-groups/README.md) removes, after
which a large block is a legal large frame. This needs a small block reader
beside ebml-iterable for the block header and payload; prefer an upstream
ebml-iterable option if one fits. Ignored elements are discarded as their
bytes arrive instead of buffered.

Open, for the maintainer before building: `Import::new` decodes `Info`,
`Tracks`, and `TrackEntry` as whole masters, so a header declaring a huge one
still pins input. Bring options with a recommendation, such as handling
their children incrementally as the importer already does for Clusters; a
blanket tag-size cap is ruled out above. Tests feed a huge declared block,
metadata master, and ignored element in chunks and assert held bytes stay
bounded. The fMP4 importer answered the same question by refusing a `moov`,
`moof`, or text sample over 32 MiB at its header (maintainer, 2026-10-31,
[moq-dev/moq#7232](https://github.com/moq-dev/moq/pull/7232)).

Public API: none expected. Wire: none.
