# [S] js/publish advertises the full codec string

## Goal

Every video rendition `@moq/publish` puts in the catalog carries a full
RFC 6381 codec string, so native players can decode what browsers publish.
Today the encoder probe in `js/publish/src/video/encoder.ts` falls back to the
bare hints `vp09`, `avc1`, `av01`, and `hev1` when no specific string is
supported, and the catalog publishes the hint as the codec. Rust `hang` parses
those as `VideoCodec::Unknown`, so moq-video and the other native consumers
refuse the track ([#4095](https://github.com/moq-dev/moq/pull/4095)).

## Plan

Decided: advertise the codec string from the encoder's own output,
`EncodedVideoChunkMetadata.decoderConfig.codec`, rather than the probe input.
Rust keeps refusing bare hints: the decoder gate needs the profile before it
subscribes.

Guidance:

- The catalog is built from the resolved config today, before any frame is
  encoded. Either hold the rendition out of the catalog until the first
  output reports its `decoderConfig`, or update it then; keep the stall and
  jitter reporting working either way.
- Check what each browser returns for a bare hint. If one echoes the hint
  back, derive the string from the bitstream (SPS for H.264/H.265, the
  sequence header for AV1, the uncompressed header for VP9), or fail loud
  rather than publish a string no native player accepts.
- A reconfigure (resolution or codec change) can change the string; the
  catalog follows it.
- Tests with the fake `VideoEncoder`: a bare-hint probe whose output reports
  a full string publishes the full string.
