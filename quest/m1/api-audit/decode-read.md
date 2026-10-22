# [M] One decode read

## Goal

moq-audio and moq-video decoders yield the same read as moq-mux containers,
`decode::Read { Frame, Gap, End }`, so a player handles loss and the end of
a broadcast the same way for every media type.

## Plan

Decided 2026-10-22 (API audit). Today `moq_mux::container::Read` is
`{Frame{group}, Gap, End{group, end}}`, `moq_audio::decode::Read`
(`decode/consumer.rs`) signals loss with a `discontinuity` counter, and
`moq_video::decode::Consumer::read` returns a bare `Frame`, losing gaps and
the end while its docs call it a mirror of audio.

- One shape modeled on moq-mux's, shared by audio and video decode.
- Callers: moq-c `audio.rs`, `moq play`, moq-ffi media readers.

Public API: moq-audio, moq-video, moq-c breaking. Wire: none.

## Related

- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - adjacent decode work
