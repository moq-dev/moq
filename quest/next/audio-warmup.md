# [S] Audio warmup: Opus converges before a joined viewer hears it

## Goal

A viewer joining an Opus rendition mid-stream, or skipping within it, never
hears the decoder's first unconverged output. Publishers set the rendition's
`warmup` to the Opus pre-roll (80 ms, RFC 7845 section 4.6), import sets it
for Opus tracks, and both audio consumers join that much earlier and discard
decoded samples stamped before the join group's start plus `warmup`. AAC-LC
frames decode independently and set nothing; HE-AAC is out of scope.

## Plan

- `rs/moq-audio/src/encode/producer.rs` and `rs/moq-mux/src/codec/opus`
  publish `warmup` for Opus; `js/publish` does the same for its Opus track.
- `rs/moq-audio/src/decode` already trims Opus `pre_skip` at stream start;
  the warmup trim is the same mechanism keyed on the container consumer's
  non-continuous signal, and the subscription's maximum age grows by `warmup`
  as the video consumer quest does. `js/watch` audio mirrors it.
- Tests in both languages: a mid-stream join discards exactly the warmup span
  and a continuous listener loses nothing.

## Required

- [Catalog warmup](/quest/next/catalog-warmup.md) - the field this reads and writes

## Related

- [Intra-refresh GOPs](/quest/future/intra-refresh/README.md) - the video side of the same field
