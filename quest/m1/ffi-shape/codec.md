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

libmoq's codec symbols follow.

Public API: breaking in every binding and libmoq. Wire: none.

## Required

- [JSON](/quest/m1/ffi-shape/json.md) - sets the per-language namespace pattern
- [Media](/quest/m1/ffi-shape/media.md) - the catalog handle the encoders register into
