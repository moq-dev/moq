# [M] An encode backend seam and an AAC output codec

## Goal

`moq_audio::encode` selects a backend the way `moq_video::encode` does, and
`Codec::Aac` is a valid output: AAC-LC at the input's sample rate and layout,
published with the AudioSpecificConfig description every player expects. A
host with no AAC encoder refuses it at construction.

## Plan

Mirror the decode seam: `encode::backend` with a crate-private `Backend`
trait (`encode`, `flush`, `set_bitrate`, `name`), an `open(codec, config)`
that walks platform candidates before software ones, and `encode::Kind` on
`encode::Config`. Opus and PCM move behind the trait unchanged and remain the
only software backends: no Rust AAC encoder exists, which is why the platform
quests follow.

- `encode::Codec` gains `Aac`, meaning `mp4a.40.2`. `as_str` and `FromStr`
  accept `"aac"`, which is what the FFI and libmoq codec strings carry, so
  moq-ffi and every binding gain AAC by string with no signature change.
- Catalog emission: `AudioCodec::AAC` with profile 2, the ASC as
  `description`, `Container::Legacy`, and the encoder's reported delay folded
  into timestamps like Opus pre-skip is today.
- Frame size is the codec's (1024 samples for AAC), so `frame_duration` is
  validated per codec rather than against the Opus table.
- Bitrate updates go through the backend; one that cannot change rate
  mid-stream keeps its opening rate, as the video seam documents.
- Regression: the selection order with a stub backend; `Codec::Aac` refused on
  a host with no backend; the Opus and PCM paths unchanged.

## Required

- [Decode seam](/quest/m2/audio-codecs/decode-backend.md) - the naming and shape this mirrors

## Related

- [OBS audio publishing](/quest/m2/obs-moq-video/audio-publish.md) - the OBS encoder adapter can offer AAC once this lands
