# [M] fMP4 import streams mdat

## Goal

The fMP4 importer's memory follows its output's backpressure, not the sizes
an input declares. A box header claiming 2^62 bytes, or an mdat that never
completes, cannot grow the importer's buffer without bound, including across
HLS segments.

## Plan

The 2026-10-27 audit (MUX-2) found whole-box decoding in
`rs/moq-mux/src/container/fmp4/import.rs`: an incomplete box retained all
subsequent input until its declared size arrived. HLS carries an incomplete
box across segment boundaries, so one bogus header could accumulate every
later segment. This checkpoint removes that wait from media and ignored
boxes; metadata and subtitle accumulation remain below.

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

Decided 2026-10-29 (maintainer implementation brief, with AFK covering
recommended code choices): refuse backwards per-track byte ranges before
publishing any fragment payload. Forward gaps and interleaved tracks remain
supported. A sequential MoQ group writer cannot stream decode-ordered
samples whose physical bytes arrive backwards without retaining earlier
payload. This settles the draft's remaining input-contract decision.

Implemented on the PR branch (checkpoints of 2026-10-29 and 2026-10-31):

- Top-level headers are read incrementally. Ignored bodies are discarded as
  input arrives, including across `cut()` calls; a zero-sized ignored box
  extends to EOF. Discarded bytes count toward moof-relative offsets.
- `begin_mdat` resolves selected media and text samples before opening media
  writers, and releases the catalog reservation only once a fragment
  anchors. The streaming resolver keeps `Claimed` checks and rejects
  backwards ranges per track; complete-frame CMAF decoding keeps its
  random-access behavior.
- Media samples stream into MoQ frame writers. Passthrough writes rewritten
  per-track moof+mdat headers, sized from the real mdat header encoding
  (largesize past 4 GiB) with checked `data_offset` values, then each
  track's sample bytes into its own writer. Gaps and unused trailing mdat
  bytes are discarded.
- While a fragment publishes, its open frames own their groups, so `seek()`
  and `discontinuity()` wait for its last sample. A refused fragment aborts
  its open frames and discards the rest of its mdat. EOF inside a sample
  aborts that frame, failing its group, finishes every track, and returns
  `UnexpectedEof`.
- Tests cover backwards-range refusal before output, huge/EOF-sized mdat with
  output-owned samples and cache accounting, repeated cuts, ignored boxes,
  deferred controls, truncation at EOF, and refused-fragment recovery.

Open question, 2026-10-31: whole-box metadata and staged text samples. A
`moov` or `moof` body still buffers whole, and each text sample stages in a
`BytesMut` before rewriting. Both are held before anything can publish, so
incremental parsing alone cannot bound them: a structurally valid `trun`
(sample count up to 2^32, 16 bytes per entry) or a text sample (size up to
4 GiB) retains every later segment as parsed state. Bounding them needs a
limit somewhere: on the held body, or on the counts and open-ended fields an
incremental parser would keep. The PR's `## Decisions needed` asks which.

Remaining work:

- Bound metadata and text staging as decided, with huge `moov`/`moof`,
  `trun`, and text-sample tests that run in CI.
- Run the streamed-import chunk-size benchmarks against `main`, `just ci`,
  and the cross-vendor review. Keep the MKV implementation independently
  reviewable.

Public API: no signature change. Adds `Error::BackwardsDataOffset` to the
non-exhaustive error enum; streaming import now refuses backwards per-track
layouts, so such input must be repackaged in forwards per-track byte order.
Wire: none.

## Related

- [MKV import streams blocks](/quest/m1/security-audit/mkv-stream-blocks.md) - the same principle for MKV
- [Media fuzz](/quest/m2/media-fuzz.md) - would catch this class of buffer lifetime bug
