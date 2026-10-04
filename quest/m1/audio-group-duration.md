# [M] Audio publishers can pack several frames per group

## Goal

`@moq/publish` and `moq-audio`'s encode Producer take a minimum audio group
duration. A group is cut at the first frame that reaches it, so a relay pays
one stream and one group's bookkeeping per N frames instead of per 20 ms
frame. The default stays 0, today's one frame per group.

## Plan

`js/publish/src/audio/encoder.ts` writes each Opus frame as its own group, and
the reporter measured relay CPU and memory scaling with audio groups (about
50 per second per track per subscriber). Rust's Opus import already
accumulates frames until the caller cuts.

Decided (2026-10-04):

- Minimum only. Opus frames are a fixed size, so a maximum adds nothing until
  there is an opportunistic cut point (silence, a video group start).
- JS and Rust, with mirrored names; the bindings pick it up through the FFI
  shape work.
- Default 0. A caller raises it, for example when its viewers already buffer
  for jitter.
- Measure relay cost and PLC under loss at 0, 100, and 200 ms with the
  existing relay bench, and record the numbers here.

Follow-up decision after the measurement: whether the minimum should be
derived from a viewer latency or jitter hint instead of set by hand. Grouping
adds no delay normally, since frames forward within a group, and costs
head-of-line blocking only on loss, which a buffer an RTT deep absorbs.

## Closes

- [#4784](https://github.com/moq-dev/moq/issues/4784) - close this issue when the quest finishes

## Related

- [Group cost](/quest/m1/perf/group-cost.md) - makes each group cheaper at the relay
- [Watch jitter target](/quest/m0/audio-jitter-target/watch.md) - lists a group per audio frame as a suspect
