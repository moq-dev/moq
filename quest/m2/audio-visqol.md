# [M] Nightly grades audio quality with ViSQOL

## Goal

The audio-quality harness scores how decoded audio sounds, not only whether
it glitched. Each graded row compares the played output against a reference
clip with ViSQOL's full-band audio mode, and the nightly fails when a score
drops below its measured floor, so a codec, bitrate, resampler, or channel
regression goes red without anyone listening.

## Plan

Decided in planning (2026-10-20):

- Perceptual scoring is in scope; the harness README lists it under "Not
  covered" today.
- Grader: the `visqol-rs` crate (Apache-2.0, pure Rust). First verify it has
  ViSQOL v3's 48 kHz audio mode and matches the C++ reference on its
  conformance set; its README documents wideband speech. If it does not,
  stop and propose a course change to the maintainer (the alternatives
  considered were `visqol-python`, which claims C++ parity, and the Bazel C++
  build).
- Per-row floors live in `budgets.json` and are enforced like the existing
  ceilings, measured from nightlies by the same rule. Native `pulse` floors
  stay report-only until [Audio outlives
  video](/quest/m2/audio-bufferbloat-av.md) makes those rows enforceable.

Recommendations:

- ViSQOL says little about a pure sine, so score a freely licensed music or
  speech clip; check its license before committing it. Mind the harness's
  silence classifier (below -60 dBFS): a clip with quiet passages may need
  its own rows.
- Score both browser rings and the native `pulse` rows. Resample AAC's
  44.1 kHz output to 48 kHz. ViSQOL aligns, but not across a skip, so score
  the contiguous segments between skips; time lost to a skip stays graded by
  the existing skip-ahead and discarded-duration budgets, not hidden in the
  score.
- Speech-only metrics (PESQ, DNSMOS, SQUIM) work at 16 kHz and would hide
  full-band codec damage; POLQA is commercial. Do not substitute them.

Public API: none. Wire: none.

## Related

- [Audio outlives video](/quest/m2/audio-bufferbloat-av.md) - the other extension of the same harness
- [Opus backends](/quest/m2/audio-opus-backend.md) - a quality score is the evidence an implementation comparison needs
- [Opus bandwidth grant](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - bitrate changes this score should track
