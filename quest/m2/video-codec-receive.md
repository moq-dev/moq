# [L] moq-video codecs split submission from receive

## Goal

moq-video's encoders and decoders take the same send/receive shape moq-audio took in [moq-dev/moq#8270](https://github.com/moq-dev/moq/pull/8270): `encode`/`decode` only submit and return `Result<()>`, `receive` pops one ready output (`Ok(None)` when nothing is ready), and `flush` marks the end of input so `receive` drains what the codec held.
No call allocates a `Vec` just to hand back its outputs.

## Plan

- Decided 2026-11-08 (maintainer, on #8270): the FFmpeg/MediaCodec split, rejecting a caller-owned `&mut Vec` buffer and keeping `Vec`. moq-video still returns `Vec` from its backend traits, `encode::Sink`, and the decoder front end, so the two crates disagree until this lands.
- Apply it to every backend (VA-API, NVDEC, Media Foundation, VideoToolbox, MediaCodec, openh264, and the rest under `rs/moq-video/src/{encode,decode}/backend/`), the front ends, and their tests. A backend that readies output synchronously holds it in a reused `VecDeque`.
- Mirror it through moq-ffi (`MoqVideoEncoder.encode`/`flush`/`finish`), its Python, Swift, Go, Dart, and Kotlin wrappers, moq-c (`moq_video_encoder_encode`/`flush`, with a `moq_video_encoder_receive` like `moq_audio_encoder_receive`), and the OBS video encoder.
- Benchmark the encode and decode paths base vs branch; allocations should drop or match.

## Related

- [moq-dev/moq#8270](https://github.com/moq-dev/moq/pull/8270) - the moq-audio change whose shape this copies
