# [XS] The JS player keeps a track's start when decoding from its first packet

## Goal

`@moq/watch` trims an audio track's catalog `warmup` only on a mid-stream
join, as moq-audio does: decoding from a track's first packet (group 0)
drops only the codec's pre-skip, so a recording's first 80 ms of Opus plays
and a short finished track is not empty.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7384](https://github.com/moq-dev/moq/pull/7384): m2, as
recommended.

[moq-dev/moq#7384](https://github.com/moq-dev/moq/pull/7384) (open) stops `moq_audio::decode::Consumer` from arming the warmup when
the first packet read is frame 0 of group 0. JS arms it unconditionally:
`js/watch/src/audio/terminal.ts:76` sets the trim boundary to the first
frame's timestamp plus warmup, fed from `audio/decoder.ts:474,562`.

- First confirm the JS behavior with a test (decode a short Opus track from
  group 0 and count frames). If it trims, apply [moq-dev/moq#7384](https://github.com/moq-dev/moq/pull/7384)'s rule: no warmup when
  the first packet is group 0's first frame; a loss or a refused first
  packet still arms it.
- Mirror [moq-dev/moq#7384](https://github.com/moq-dev/moq/pull/7384)'s tests, including a finished track shorter than the warmup.

Public API: none (behavior change in `@moq/watch`). Wire: none.

## Related

- [Opus pre-skip](/quest/m1/opus-preskip.md) - [moq-dev/moq#7384](https://github.com/moq-dev/moq/pull/7384) lands the Rust half
