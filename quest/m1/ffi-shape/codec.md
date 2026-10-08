# [L] Audio and video codecs get their own namespaces

## Goal

`audio` and `video` each own an encoder and a decoder constructed from the
handles moq-audio and moq-video take (the broadcast and its catalog), with one shape between them. `BroadcastProducer` loses
`encode_audio`/`encode_video` and `BroadcastConsumer` loses
`decode_audio`/`decode_video`.

## Plan

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
their callers construct, not one they are asked to match, so
[intra-refresh bindings](/quest/m3/intra-refresh-bindings.md) adds the refresh
variant additively instead of breaking `gop` a second time. Go gets no uniffi
default, so its zero value must read as keyframe mode.

The audio and video frame and decoder-output records carry microsecond fields
(`timestamp_us`, `max_delay_us`, `frame_duration_us`); in Python and Go they
should become owned `timedelta` / `time.Duration` records like net's.

Public API: breaking in every binding. Wire: none.
