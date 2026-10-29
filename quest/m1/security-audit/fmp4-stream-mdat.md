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

Decided 2026-10-28 (AFK implementation choice, explicitly authorized in the
implementation brief): parse metadata children incrementally so a `moov` or
`moof` declaration cannot force whole-box buffering. Do not introduce a
blanket byte cap. Apply the same principle to
[MKV import streams blocks](/quest/m1/security-audit/mkv-stream-blocks.md).

The draft adds a hostile ignored-box test that expects no retained input
across chunks and segment boundaries. It has no production fix yet: the
reproduction run could not enter the host's build gate and was stopped
under the AFK instruction to stop stalled work. The test is expected to fail
against the current importer; it has not been executed.

Remaining implementation:

- Read top-level headers incrementally and discard ignored bodies directly
  from input. Count discarded bytes for moof-relative offsets, preserve
  segment boundaries, and treat a zero-sized box as extending to EOF rather
  than the end of a decode call. Cover split headers, malformed sizes, and
  recovery at the next box boundary.
- Parse metadata containers and their children incrementally, retaining only
  fields used for track setup and sample resolution. Variable-sized leaves
  need structural parsing too; moving the whole-box wait down one level is
  not sufficient. mp4-atom 0.16.2 and upstream `main` expose header decoding,
  but their body readers still buffer whole bodies.
- Resolve every sample before publishing payload, preserving `Resolver` and
  `Claimed` checks, then stream media into output frames, discarding gaps and
  unselected samples. Preserve passthrough timing, edits, grouping, catalog
  estimation, HLS records, and text reconstruction.
- Settle non-monotonic byte layouts: the resolver currently accepts disjoint
  runs whose byte order differs from decode order. A MoQ group permits only
  one unfinished frame, so direct streaming cannot publish those samples in
  decode order without retaining earlier input. Recommend refusing backwards
  per-track ranges before publishing any fragment payload; interleaving
  tracks and forward gaps remain supported. This input-contract decision is
  left for review, not silently applied by the draft.
- Add huge metadata/mdat, unfinished-sample, truncation, and cross-segment
  tests against retained input and output-owned bytes, plus streamed-import
  benchmarks. The existing frame cap remains owned by the unbounded-groups
  work; do not substitute it for streaming.

Keep `sample::Resolver`'s range checks and `Claimed`'s no-reuse rule. Tests
feed a huge declared `moof`, `moov`, `mdat`, and skipped box in small chunks
and assert the importer's held bytes stay bounded.

Public API: none expected. Wire: none.

## Related

- [MKV import streams blocks](/quest/m1/security-audit/mkv-stream-blocks.md) - the same principle for MKV
- [Media fuzz](/quest/m2/media-fuzz.md) - would catch this class of buffer lifetime bug
