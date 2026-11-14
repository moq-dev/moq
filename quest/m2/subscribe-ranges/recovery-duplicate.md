# [S] A recovered run is served once

## Goal

A lite-07 relay's copy never receives the same group on both a recovery and
its demand subscription. Today, in Rust and JS, a recovery's stale groups can
race a SUBSCRIBE_UPDATE that reopens the same stale runs on the copy's demand
subscription. The publisher then serves the group on both, and the relay
resets the duplicate stream (code 0) for a group it already holds. No reader
is affected, but each occurrence costs a stream and puts a reset on the wire.

## Plan

Found by [moq-dev/moq#8616](https://github.com/moq-dev/moq/pull/8616), which
ported the Rust `Recovery` (`rs/moq-net/src/lite/subscriber/fills.rs`) to JS
(`js/net/src/lite/fills.ts`, `#recover` in `js/net/src/lite/subscriber.ts`).

Settled on #8616 (maintainer, 2026-11-12), which a fix keeps:

- ✅ A recovery asks with no max delay, in Rust and JS (rejected: asking with
  the most tolerant budget among the fills owing the run). A narrow reader
  therefore takes a run's stale history once every hop forgot it.
- ✅ A narrow reader may take stale groups of a recovered run (rejected:
  judging a recovery against the whole track, which needs a wire signal or a
  change to how a ranged subscription resolves its start).

Reproduce the race in a two-hop test first, in both languages, then fix it at
the root: either the recovery's run stays out of what the update reopens, or
the copy settles which subscription delivers a group before both send it.
Keep Rust and JS in parity. Wire: none expected.
