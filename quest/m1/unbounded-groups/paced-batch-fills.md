# [M] Paced batch fills

## Goal

A `track::Requests` fill that writes a whole batch of groups at once no longer
evicts its own earlier groups before the reader takes them. A reader of a
batch fill reads every group it asked for, at its own pace, without the cache
holding the whole batch.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): split from
[moq-dev/moq#7944](https://github.com/moq-dev/moq/pull/7944), which landed
flights, into a quest of its own. The failure crosses groups, while a flight
paces one group copy by its riders, and pacing the batch changes what
`Fill::wanted` promises every handler.

The follow-up from [moq-dev/moq#7339](https://github.com/moq-dev/moq/pull/7339),
moved to flights from
[fastest-reader pacing](https://github.com/moq-dev/moq/pull/7664) on
2026-11-02 (maintainer). Recommendation from #7944: `Fill::wanted` yields the
next group only once the readers took the one before, so a handler serving a
batch writes it a group at a time.

Public API: `Fill::wanted`'s contract for handlers, with a `MIGRATE.md` note.
Wire: none.
