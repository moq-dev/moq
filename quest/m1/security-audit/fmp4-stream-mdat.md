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

Implementation checkpoint, 2026-10-29:

- Top-level headers are read incrementally. Ignored bodies are discarded as
  input arrives, including across `cut()` calls; a zero-sized ignored box
  extends to EOF. Discarded bytes count toward moof-relative offsets.
- `begin_mdat` resolves selected media and text samples before opening media
  writers. The importer's streaming resolver preserves `Claimed` checks and
  rejects backwards ranges per track; the complete-frame CMAF decoder keeps
  its existing random-access behavior.
- Media samples stream into MoQ frame writers. Passthrough writes rewritten
  per-track moof+mdat headers first, then each track's sample bytes directly
  into its own writer. Gaps and unused trailing mdat bytes are discarded.
- Text resolution is separated from reconstruction, but each text sample
  still accumulates in a `BytesMut` before rewriting. `Body::Metadata` still
  buffers whole `moov` and `moof` bodies. These remain unbounded input-owned
  paths: this checkpoint does not resolve MUX-2 and must stay a draft.
- Added tests cover backwards-range refusal before output, huge/EOF-sized
  mdat with a partially output-owned sample and discarded trailing bytes,
  cache accounting, and repeated cuts. The existing ignored-box hostile
  test remains. The interleaved-run benchmark now sweeps input chunk size
  as well as track, run, and sample counts. These assertions and benchmarks
  have not yet run successfully.

Remaining work:

- Parse metadata containers and children incrementally, retaining only
  fields used for track setup and sample resolution. Variable-sized leaves
  need structural parsing too; moving the whole-box wait down one level is
  insufficient. mp4-atom 0.16.2 exposes incremental header decoding but its
  body readers buffer whole bodies. No blanket byte cap.
- Remove input-owned accumulation of subtitle samples while preserving VTT
  and TTML reconstruction. Output ownership must bound staging and any
  rewriting expansion. Do not substitute the transport's current frame cap
  for source-level memory bounds; the unbounded-groups quest owns that cap.
- Verify abort, truncated headers/metadata/samples, explicit EOF, and
  seek/discontinuity while a frame holds its group's writer. In particular,
  ensure controls cannot open a replacement group underneath an unfinished
  sample. Validate passthrough header sizing and offset arithmetic too.
- Reproduce the hostile ignored-box failure against the pre-fix source, then
  run the focused fMP4 tests, source memory/cache assertions, shared producer
  tests, streamed-import benchmarks, and `just ci`. Add huge `moov`/`moof`
  and structural-leaf tests with the metadata implementation. Every case
  must run in CI. Keep the MKV implementation independently reviewable.

Validation at this checkpoint: the first focused compile was admitted and
reported missing `Clone + Send` bounds on the private streaming producer
implementation; those bounds are now corrected. Its rerun, the baseline
reproduction, and `just ci` remained at the host build gate and were stopped
for handoff. `quest check`, `quest check --base origin/main` (664 documents),
`cargo fmt --all --check` in the Nix environment, and `git diff --check`
passed. No passing build, test, benchmark, or memory measurement is claimed.

Public API: no signature change. Adds `Error::BackwardsDataOffset` to the
non-exhaustive error enum; streaming import now refuses backwards per-track
layouts, so such input must be repackaged in forwards per-track byte order.
Wire: none.

## Related

- [MKV import streams blocks](/quest/m1/security-audit/mkv-stream-blocks.md) - the same principle for MKV
- [Media fuzz](/quest/m2/media-fuzz.md) - would catch this class of buffer lifetime bug
