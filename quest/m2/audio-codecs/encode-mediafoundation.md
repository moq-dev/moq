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
- The output type's `MF_MT_USER_DATA` carries the ASC for the catalog.
- Round-trip regression through the Media Foundation decoder; verification on
  a Windows host, since the per-PR CI compiles only.

## Required

- [Encode seam](/quest/m2/audio-codecs/encode-backend.md) - the candidate order this backend joins
- [Layout](/quest/m2/audio-codecs/layout.md) - the input layout the encoder accepts

## Related

- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - where the Windows run happens
