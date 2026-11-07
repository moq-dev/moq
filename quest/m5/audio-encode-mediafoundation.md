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

Changed 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): [audio-codec-seam](/quest/m2/audio-codec-seam.md) makes moq-audio's codecs return zero or more outputs with `flush` before the cut, so this backend adds no break.

## Required

- [AudioToolbox encode](/quest/m5/audio-encode-audiotoolbox.md) - changes `Encoder::encode` to a zero-or-more packet return with a flush, which this backend implements
- [Media Foundation decode](/quest/m5/audio-decode-mediafoundation.md) - the round-trip regression decodes through it

