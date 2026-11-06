# [M] Run-based relay fills are paced

## Goal

A relay pacing a run-based fill (a moq-transport FETCH run, or a lite-07
range fill) backpressures the upstream stream once it sees a downstream reader
lose a batch to eviction, so a slow reader of a long run stays within the
tail instead of falling off it.

## Plan

Added 2026-11-06 (maintainer, follow-up planning in `/quest-complete`), from
[moq-dev/moq#8017](https://github.com/moq-dev/moq/pull/8017), which paces a
batch fill's handler by its fastest reader. Run-based fills still read
upstream at full speed.

Pace on evidence, not up front: an unpaced run is fine while every reader
keeps up, so start backpressure only after the relay sees a downstream
reader lose a batch to eviction, then hold the upstream stream as #8017
holds a batch handler. Test a slow reader of a long FETCH run and of a
lite-07 range fill through a relay.

Public API: none expected. Wire: none.
