# [S] moq play plays a finished track's last samples

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
per iteration and never settles. Fix it at the sink instead of the poll
(maintainer decision, 2026-09-28): `Sink::finish` consumes the sink and
returns a `Drain` that resolves once the mixer has played everything the sink
held, then leaves the mix. `drain` awaits it and loses its threshold. Lowering
the threshold to zero is rejected: it is the wakeup spin above. The change to
moq-audio's playback API is additive; dropping a sink still leaves the mix
immediately.

Tighten `an_audio_rendition_switch_leaves_no_gap` (the `play::fake::Recorder`
already models the cut on drop) so the lost tail fails it, and add a finite
track that asserts its final samples are heard.
