# [M] fMP4 import streams mdat

## Goal

The fMP4 importer's memory follows its output's backpressure, not the sizes
an input declares. A box header claiming 2^62 bytes, or an mdat that never
completes, cannot grow the importer's buffer without bound, including across
HLS segments.

## Plan

From the 2026-10-27 audit (MUX-2), verified worse than reported:
`rs/moq-mux/src/container/fmp4/import.rs` appends input to `self.buffer` and
waits for `mp4_atom::Any::decode_maybe` to see a whole box. A declared size
past what is buffered keeps returning `None`, and the HLS importer carries a
trailing partial atom into the next segment, so one bogus header accumulates
every later segment.

Decided 2026-10-27 (maintainer): no byte cap. Stream each mdat sample into
its frame as bytes arrive, resolving sample ranges from the parsed `moof` as
today, and discard skipped boxes (`free`, `skip`, unknown types) as their
bytes arrive instead of buffering them. This fits
[unbounded groups](/quest/m1/unbounded-groups/README.md), which removes the
frame cap so a sample can be large. Since
[8d2fa6653a](https://github.com/moq-dev/moq/commit/8d2fa6653a) a fragment can
hold several GOPs, so a fragment-sized cap would also refuse legal input.

`moov` and `moof` are parsed whole today, so a header declaring a huge one
still pins input. Decided 2026-10-28 (maintainer, for both importers): parse
metadata children incrementally, keeping only the fields consumed, with no
blanket byte cap. MKV import does so since
[moq-dev/moq#7234](https://github.com/moq-dev/moq/pull/7234).

Keep `sample::Resolver`'s range checks and `Claimed`'s no-reuse rule. Tests
feed a huge declared `moof`, `moov`, `mdat`, and skipped box in small chunks
and assert the importer's held bytes stay bounded.

Public API: none expected. Wire: none.

## Related

- [Media fuzz](/quest/m2/media-fuzz.md) - would catch this class of buffer lifetime bug
