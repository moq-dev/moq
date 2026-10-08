# [S] Audio groups span at least 20 ms by default

## Goal

An audio publisher that never sets a group duration emits at most about 50
groups per second, whatever its codec frame size. Today every producer
defaults `group_duration` to zero, a group per packet, so the go interop
client's 2.5 ms Opus frames mint about 400 groups/s, each a QUIC stream and a
serve.

## Plan

Decided 2026-10-08 in a `/quest-plan` interview (paper trail in the PR that
added this quest):

- `moq_audio::encode::Options::group_duration` defaults to 20 ms
  (`rs/moq-audio/src/encode/producer.rs`, documented today as "Defaults to
  zero, a group per packet"). 20 ms frames behave as today; smaller frames
  share a group up to 20 ms.
- Every producer follows it: moq-ffi `encode_audio`, moq-c, moq-boy, and the
  CLI through `Options::default()`; JS publish's `groupDuration`
  (`js/publish/src/audio/encoder.ts`); and the GStreamer sink, which today
  cuts after every audio packet (`rs/moq-gst/src/sink/pad.rs`). Rejected:
  only the FFI sets it, and keeping zero.
- Docs: `doc/lib/rs/moq-audio.md` and the JS publish docs drop "a group per
  packet" as the default.
- Test: a 2.5 ms Opus encode yields groups of at least 20 ms in Rust and JS,
  and the gst sink's grouping test.

Public API: a changed default in moq-audio and `@moq/publish`. Wire: none.

## Related

- [Serve budget](/quest/m0/serve-budget.md) - bounds the serve loop whatever the group rate
- [FFI codec namespaces](/quest/m1/ffi-shape/codec.md) - owns the FFI encoder's frame duration default, a separate knob
