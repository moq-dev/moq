# [L] moq-ffi: a video consumer decodes a subscribed rendition on every binding

## Goal

An application built on any binding subscribes to a video rendition and
receives decoded frames, the counterpart of the existing raw video producer
and of the audio consumer. `moq-ffi` has `MoqVideoProducer`,
`MoqAudioProducer`, and `MoqAudioConsumer`; `moq-video::decode` exists with
VideoToolbox and openh264 backends; nothing joins them.

## Plan

- Add `MoqVideoConsumer` beside `MoqAudioConsumer` in `rs/moq-ffi/src`,
  selecting a rendition from the catalog, decoding with `moq-video::decode`,
  and delivering frames through the same callback-free handle shape the audio
  consumer uses. Frames cross the boundary as I420 or RGBA byte arrays with
  their timestamp and dimensions, the MVP #700 scoped; a zero-copy native
  surface waits on the ownership decision.
- Walk the Cross-Package Sync table: `rs/libmoq` and `moq.h`, the `py`,
  `swift`, `kt`, and `dart` wrappers, the Go wrapper, and `doc/lib` for each.
  The Dart binding alone has no codecs; say so rather than stubbing.
- Verify on an iOS simulator and an Android emulator with the smoke media,
  and on macOS through libmoq so `just test smoke-full` covers it.

## Related

- [Ownership boundary](/quest/m2/mobile/ownership-boundary.md) - decides whether frames stay byte arrays
- [Decoder drain](/quest/m3/decode-drain.md) - flush and finish for pipelined decoders this will lean on
