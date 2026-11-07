# [S] A bus channel reports what it holds

## Goal

A `moq_audio::bus` `Channel` reports how much PCM it holds and not yet mixed,
or where the bus will mix its next sample, so a writer that leads the bus by
a fixed latency places a first write after silence to the sample, not up to
a block early.

## Plan

From [moq-dev/studio#93](https://github.com/moq-dev/studio/pull/93), picked
2026-11-07 (maintainer away under /afk, recommended option).

The studio's `playout::Writer` writes a source's sound one bus latency
ahead. After a silence it cannot tell how much its channel holds, so it
places the first write relative to the block the bus last mixed, which can
land it up to one block (10 ms in the studio) early. Broadcast and media
sources both hit this. Pick whichever of a fill count or a bus position
makes the writer simplest, and test that a first write after silence is
mixed at the sample it was meant for.

Public API: one read-only addition to `Channel`. Wire: none.
