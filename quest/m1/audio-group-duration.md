# [M] Audio publishers can pack several frames per group

## Goal

`@moq/publish` and `moq-audio` take a minimum audio group duration, so a
relay pays one stream and one group's bookkeeping per N frames instead of per
20 ms frame. The default stays 0, today's one frame per group.

## Plan

`js/publish/src/audio/encoder.ts` writes each frame as its own group, and
`rs/moq-audio/src/encode/producer.rs` cuts after every packet. The reporter
measured relay CPU and memory scaling with audio groups (about 50 per second
per track per subscriber).

Decided (2026-10-04):

- `groupDuration?: Time.Milli` on Audio.Encoder props and `group_duration` on
  moq-audio's encode `Options`, for Opus, AAC, and PCM alike. The frame that
  reaches the minimum closes its group; the next frame opens a new one.
- Minimum only. A maximum adds nothing until there is an opportunistic cut
  point (silence, a video group start).
- Default 0. A caller raises it, for example when its viewers already buffer
  for jitter. A longer Opus `frameDuration` (60 ms) is the no-code
  alternative; document both.
- JS builds on the shared `Container.Legacy.Producer` that
  `js/publish/src/audio/encoder.ts` already writes through.
- Measure relay cost and loss concealment at 0, 100, and 200 ms with the
  existing relay bench, and record the numbers here. Longer groups also make
  a viewer's group skipping coarser; note it in the docs.

Follow-up decision after the measurement: whether the minimum should be
derived from a viewer latency or jitter hint instead of set by hand. Grouping
adds no delay normally, since frames forward within a group, and costs
head-of-line blocking only on loss, which a buffer an RTT deep absorbs.

## Closes

- [#4784](https://github.com/moq-dev/moq/issues/4784) - close this issue when the quest finishes

## Related

- [Group cost](/quest/m1/perf/group-cost.md) - makes each group cheaper at the relay
- [Watch jitter target](/quest/m0/audio-jitter-target/watch.md) - lists a group per audio frame as a suspect
