# [M] moq-ffi decoders choose where to start

## Goal

A moq-ffi audio or video decoder can join a track live instead of replaying
its cached backlog. `MoqAudioDecoderOutput` and `MoqVideoDecoderOutput` gain a
`start` (`Oldest` or `Latest`, defaulting to oldest) wired to
`moq_{audio,video}::decode::Options::start`, so a Python, Swift, Kotlin, Go,
Dart, or C++ live player that reopens a decoder skips the backlog, as a moq-c
caller does with `MOQ_DECODE_START_LATEST`.

Wire: none. Public API: every binding's decoder output gains a field.

## Plan

Decided 2026-10-29 (maintainer, settling
[moq-dev/moq#7033](https://github.com/moq-dev/moq/pull/7033)): moq-c's zeroed
`start` means oldest, and moq-ffi follows in its own quest.

- Mirror moq-c's `moq_decode_start` names (`Oldest`, `Latest`) in a UniFFI
  enum, and keep oldest the default so existing callers are unchanged.
- Update the hand-written wrappers (`py/moq-rs`, `go/wrapper`, `dart/moq`,
  `cpp/moq`, Swift, Kotlin) and `doc/lib/{py,swift,kt,go,dart,cpp}`; each
  live-player sample says when to pick latest.
- Test that a finished track decodes frames with the default and that latest
  joins at the newest group.
