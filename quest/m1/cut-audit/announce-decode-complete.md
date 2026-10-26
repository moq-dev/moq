# [S] A malformed lite message closes the session instead of jamming its stream

## Goal

A lite ANNOUNCE body whose fields run past its own declared length closes the
session with PROTOCOL_VIOLATION, as JS already does, instead of leaving the
announce stream waiting for bytes that will never come. Lite fuzzing asserts
that no complete body ever decodes as `Short`, and covers the lite-07 TRACKS
and AUTH decoders.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Codec half reproduced
(`decode_slice` returns `Short` for `[0,1,5,..]` on lite-06/07 and `[1,1]` on
lite-05); the jam is source-inferred. Inherited.

`lite::Announce::decode` (`rs/moq-net/src/lite/announce.rs:340-381`) decodes
the lite-06+ body and the legacy body inside `buf.sub(size)` without
`DecodeError::complete`, unlike `message.rs` and `tracks.rs`. A Hop Count
larger than the hops that follow returns `Short`; `Reader::poll_with_sized`
(`coding/reader.rs:147`) then grows its window a byte at a time, and
`lite/error.rs:34` deliberately never maps `Short` to a violation. Every
later announcement re-runs the failed decode until FIN.

- Decode both bodies with `.map_err(DecodeError::complete)`, or through
  `message::decode_with`. Grep the lite and IETF codecs for any other `sub()`
  body decoded without it.
- In `fuzz.rs` `roundtrip`, assert `Err(Short)` implies the decoder needed
  more bytes than it was given, so this class fails the fuzzer.
- Add `lite_wire` arms for `Tracks`/`TracksEvent` and
  `Auth`/`AuthOk`/`AuthError`/`AuthReply`, all peer-received lite-07
  decoders with no target today.

Public API: none. Wire: none.

## Related

- [Wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) - the remaining decoder and unsafe targets
