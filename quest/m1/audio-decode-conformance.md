# [L] Native audio decode passes a conformance suite like moq-video's

## Goal

Every native audio decode path is checked against a known input, the way
`rs/moq-video/src/decode/conformance.rs` checks video against reference YUV.
Opus and AAC decoded through each moq-audio backend come out at the right
frequency, level, channel order, sample count, and timestamps, and the
backends agree. The C and FFI audio decode surfaces are exercised by tests,
not only by applications.

Today `rs/moq-audio/src/decode/backend/symphonia.rs` (AAC) has no tests, the
Opus round trip asserts only frame count and energy above 0.01, and
`moq_decode_audio`, `moq_decode_audio_frame`, and their free function in
`rs/moq-c` are never called by a test.

## Plan

Decided in planning (2026-10-20): both synthesized tones and real clips.

- Tones test our wrapping logic: a distinct frequency per channel (catches
  channel order and remix), a stepped sweep (catches dropped or duplicated
  frames), and timestamp continuity across pre-skip, resampling (48 kHz and
  44.1 kHz), and surround layouts. Backends return samples only; the
  consumer assigns timestamps, so continuity is checked at the consumer.
  libopus has its own test vectors; this suite tests what moq-audio does
  around it.
- Real clips catch what tones miss: the AAC in `rs/moq-mux`'s `bbb.mp4`
  fixture, plus a new small Opus fMP4/CMAF fixture (the repo has none; Opus
  fMP4 tests synthesize their input today). Opus and AAC decoders are not
  bit-exact across implementations, so compare against a reference decode
  from a decoder independent of every backend under test (so a shared bug
  cannot make them agree), with a tolerance derived from measured backend
  differences, and say how it was measured. Commit each encoded fixture
  with the command that produced it.
- Folded in from the same gap analysis:
  - moq-ffi gains audio counterparts of the video raw-publish tests (publish
    from many threads, reject bad frames), and its decode-delay test feeds a
    tone instead of silence and checks it.
  - `rs/moq-c` gains a C fixture for the audio decoder-output contract,
    alongside `c-tests/decode-output.c`.
  - `cpp/moq` gains an audio publish and consume test.

Recommendations:

- Reuse the Goertzel check `rs/moq-audio/src/decode/backend/libopus.rs`
  already uses for surround channel order, rather than adding an FFT
  dependency.
- Upgrade `tests/integration/roundtrip.rs` in place instead of adding a second
  round trip.
- A tolerance or threshold needs a stated reason; avoid round numbers picked
  to pass.

Public API: none expected. Wire: none.

## Related

- [Wire-compat audio](/quest/m1/wire-compat-audio.md) - the same decoders, end to end across releases
- [AudioToolbox decode](/quest/m2/audio-decode-audiotoolbox.md) - a new backend that should join this suite when it lands
- [Opus backends](/quest/m2/audio-opus-backend.md) - compares Opus implementations; this suite is its correctness floor
