# [XS] An audio track's final flush joins its open group

## Goal

`moq_audio::encode::Producer::finish` publishes the encoder's final flush in
the track's open group instead of starting a new one, so a subscriber with the
default zero `delay.max` receives the tail of the audio rather than an `Old`
error. If upstream `main` already does this, record that and delete the quest.

## Plan

From moq.pro's voice demo flake
([moq-dev/moq.pro#3318](https://github.com/moq-dev/moq.pro/pull/3318)): when the
bot disconnected, `finish()` flushed the Opus encoder into a new group. Groups
travel on separate streams, so the new group could reach a relay before the
previous group's end marker, and a zero-`delay.max` subscriber then saw the
older group as superseded and got `Old`, losing about 0.8 s of audio in about 3%
of runs. moq.pro worked around it by subscribing with a 30 s max delay.
Players set their own latency budget, so this isn't urgent. Check against
`rs/moq-audio/src/encode/producer.rs` on `main` first: the group rules there
(a discontinuity or reanchor opens a marker group) may already cover the
flush.

Checked 2026-11-12 (/quest-audit): not done. When the codec's terminal drain
carries discard padding, `publish_terminal`
(`rs/moq-audio/src/encode/producer.rs:610`) writes an empty `keyframe: true`
frame, which opens a new group for the tail. The same race can hit any group
boundary under a zero `delay.max`; the final flush is only where it costs the
most.

Moved to m4 2026-11-12 (maintainer, /quest-audit): no consumer waits on it, since the downstream deployment worked around it.
