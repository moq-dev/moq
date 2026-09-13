# [L] moq-ffi: a video consumer decodes a subscribed rendition on every binding that ships codecs

## Goal

An application built on a binding whose artifacts carry codecs subscribes to
a video rendition and receives decoded frames, the counterpart of the
existing raw video producer and of the audio consumer. Dart participates when its shipped artifacts enable codecs; check
[Dart codec parity](/quest/m2/dart-codecs.md) at implementation time. Every other
binding is done here. `moq-ffi` has `MoqVideoProducer`,
`MoqAudioProducer`, and `MoqAudioConsumer`; `moq-video::decode` exists with
VideoToolbox and openh264 backends; nothing joins them.

## Plan

Reuse `moq_video::Frame` through the shared binding ownership contract.
Rust decoding for bindings that ship codecs is independent of who eventually
owns mobile capture; native mobile surface views remain deferred.

- Add `MoqVideoConsumer` beside `MoqAudioConsumer`, selecting a rendition,
  decoding with `moq-video::decode`, and delivering through the existing
  callback-free handle style. Portable bindings receive I420 pixels with
  timestamp, dimensions, and documented plane layout using Surface's existing
  CPU conversion. Retain the underlying frame until conversion or native work
  is complete; do not create another decoder or surface abstraction.
- Walk the Cross-Package Sync table: `rs/libmoq` and `moq.h`, the `py`,
  `swift`, `kt`, and `dart` wrappers, the Go wrapper, and `doc/lib` for each.
  If Dart artifacts still lack codecs, document the temporary omission rather
  than stubbing a consumer that cannot decode.
- Verify on an iOS simulator and an Android emulator with the smoke media,
  and on macOS through libmoq so `just test smoke-full` covers it.

The later-finishing quest, this one or Dart codec parity, must expose and test
the video consumer in Dart. Neither may finish by assuming the other will
perform an untracked follow-up.

## Required

- [Decoded frame ownership](/quest/m2/decoded-frames.md) - shared frame lifetime and portable conversion contract

## Related

- [Decoder drain](/quest/m3/decode-drain.md) - flush and finish for pipelined decoders this will lean on
- [Dart codec parity](/quest/m2/dart-codecs.md) - the one binding that cannot decode until it lands
