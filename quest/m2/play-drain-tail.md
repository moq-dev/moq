# [XS] moq play plays a finished track's last samples

## Goal

When `moq play` retires an audio rendition or reaches the end of a finite
track, every sample it wrote reaches the speaker. Today `drain` in
`rs/moq-cli/src/play/media.rs` returns once 10 ms or less is buffered and
drops the sink, which removes it from the mix, so each retired rendition and
finite track loses up to its last 10 ms
([#4154](https://github.com/moq-dev/moq/pull/4154)). The rendition-switch test
tolerates the gap.

## Plan

The 10 ms stop exists because polling the last partial period costs a wakeup
per iteration and never settles. Fix it at the sink instead of the poll: let
a dropped or finished `moq_audio::playback::Sink` play out what it holds
before leaving the mix, or give it an end-of-stream that the mixer honors, so
`drain` no longer needs a threshold. If that belongs in moq-audio's playback
API, keep the change additive.

Tighten `an_audio_rendition_switch_leaves_no_gap` (the `play::fake::Recorder`
already models the cut on drop) so the lost tail fails it, and add a finite
track that asserts its final samples are heard.
