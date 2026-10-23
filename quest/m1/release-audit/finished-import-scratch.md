# [M] Release finished importer scratch

## Goal

A finished binding video producer or media importer retains the publication
and catalog ownership needed by late readers, while releasing parser input
storage that it can no longer use. Retiring more sources on a live broadcast
does not retain their obsolete parser allocations.

## Plan

Source-confirmed inventory from
[moq-dev/moq#6421](https://github.com/moq-dev/moq/pull/6421), not an RSS or
hardware-encoder exhaustion measurement. That PR releases audio codec and
PCM/conversion resources on finish. The video path already consumes its
native encoder before `Finished::keep`.

The remaining `keep` calls in `rs/moq-ffi/src/video.rs` and `media.rs` retain
whole publication/importer wrappers. Inspect the state left after successful
finish: video's `moq_video::encode::Producer::codecs` owns splitters;
`moq_mux::import::{Track, TrackStream, Container}` owns codec/container
parsers. Examples include H.264 splitter `tail` capacity and parameter-set
buffers, fMP4 `buffer` and parsed `moov`/`moof`, MKV/FLV input buffers, and TS
input scratch. Confirm which allocations actually survive EOF before
changing their owners; not every listed field necessarily retains payload.

Reproduce with retained capacity or drop-observing backing owners across
retired-source counts, not a brittle RSS threshold. Release unneeded parser
state at its owner while preserving catalog renditions, clean media tails,
late lookup/read behavior, and broadcast finish. Keeping only a raw track
clone is insufficient if dropping the wrapper also retires its catalog
rendition. Keep the independently fixed audio codec lifetime intact.

No binding API or wire-format change is expected. Current consumers are
long-lived binding broadcasts that retire video or imported media sources.

## Related

- [TS state bounds](/quest/m1/ts-reliability/state-bound.md) - bounds live SI and route revisions, distinct from finished parser storage
- [moq-dev/moq#6307](https://github.com/moq-dev/moq/pull/6307) - interop proves clean broadcast tails across implementations
