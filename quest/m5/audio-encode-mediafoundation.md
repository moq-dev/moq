# [M] Media Foundation AAC encode on Windows

## Goal

On Windows, `Codec::Aac` encodes through the Media Foundation AAC encoder
MFT, mono, stereo, and up to 5.1.

## Plan

The audio counterpart of `rs/moq-video/src/encode/backend/mediafoundation.rs`,
behind the encode seam on Windows.

- The encoder MFT fixes its output rates and bitrates per channel count;
  refuse configurations outside that table at construction rather than
  letting the MFT pick silently.
- The catalog ASC is synthesized at construction per the encode seam; the
  output type's `MF_MT_USER_DATA` must match it, asserted in tests.
- Round-trip regression through the Media Foundation decoder; verification on
  a Windows host, since the per-PR CI compiles only.

This backend implements the zero-or-more packet return and `flush` that
[audio-codec-seam](/quest/m2/audio-codec-seam.md) lands in m2 before the cut
(decided 2026-11-07, maintainer, /quest-plan from the API sweep of open PRs
and remaining quests), so it adds no break and waits on no other backend.

## Required

- [moq-audio codecs return zero or more outputs](/quest/m2/audio-codec-seam.md) - the encoder API this backend implements
- [Media Foundation decode](/quest/m5/audio-decode-mediafoundation.md) - the round-trip regression decodes through it

