# [L] Audio and video codecs get their own namespaces

## Goal

`audio` and `video` each own a broadcast-bound producer, a codec-only
encoder, and a decoder, mirroring moq-audio's and moq-video's types, with one
shape between them. `video.Producer` and `audio.Producer` mirror Rust's
`encode::Producer` and are constructed from the handles it takes (the
broadcast and its catalog). `video.Encoder` and `audio.Encoder` mirror
`encode::Encoder` and take only a codec config. `BroadcastProducer` loses
`encode_audio`/`encode_video` and `BroadcastConsumer` loses
`decode_audio`/`decode_video`. The audio encoder's frame duration defaults
to the codec's own frame in every binding.

## Plan

Decided by the maintainer in the 2026-10-06 audit: the binding names mirror
Rust, so every moq-ffi type maps to the Rust type of the same name. The
broadcast-bound types are `video.Producer`/`audio.Producer`
(`rs/moq-video/src/encode/producer.rs`, `rs/moq-audio/src/encode/producer.rs`),
and this quest also owns the codec-only `video.Encoder`/`audio.Encoder`
(`encode::Encoder` in `encoder.rs`). The OBS adapters
([video](/quest/m1/obs-moq-video/adapter.md),
[audio](/quest/m1/obs-moq-video/audio-publish.md)) consume these rather than
adding their own. Rejected: shipping only the Producers here and leaving the
codec-only Encoder to the OBS audio quest.

Mirror moq-audio's and moq-video's `encode`/`decode` modules. Today the two
disagree on where the track name goes (`encode_audio` takes it as an
argument, `encode_video` reads `output.track`), and decode passes the catalog
key apart from its rendition; pick one convention for both. Go's
`EncodeAudio` takes an options struct. Encoder producers watch subscribers
through `demand()` only. Both groups stay behind their cargo features and off
wasm.

The video encoder's output mirrors moq-video's `encode::Gop`:
`MoqVideoEncoderOutput.gop: Option<u32>` becomes a `MoqVideoGop` enum with a
`Keyframe { interval }` variant, defaulting to keyframes at two seconds, and
documented as non-exhaustive like the core. The wrappers expose it as an enum
their callers construct, not one they are asked to match. A later mode adds
its variant on this enum. Go gets no uniffi
default, so its zero value must read as keyframe mode.

The audio and video frame and decoder-output records carry microsecond fields
(`timestamp_us`, `max_delay_us`, `frame_duration_us`); in Python and Go they
should become owned `timedelta` / `time.Duration` records like net's.

Frame duration default (folded in from its own quest, 2026-10-08, so callers
take one break on the reshaped type): the audio encoder output's
`frame_duration_us` defaults to 0, the codec's own frame, instead of 20000,
so an AAC encoder works without an explicit 0 in every binding; Opus still
encodes 20 ms frames by default. Decided in
[#4183](https://github.com/moq-dev/moq/pull/4183). `rs/moq-ffi/src/audio.rs`
already treats 0 as the codec's frame; the
`default_frame_duration_matches_moq_audio` test that pins 20000 goes with the
literal. Update the wrappers and docs that restate 20000 (the Python test
asserting the default, `doc/lib/{py,swift,kt,go,dart}`). libmoq already
reads 0 as the default.

Public API: breaking in every binding. Wire: none.
