# [S] Bandwidth reservations ignore in-process readers

## Goal

An in-process reader of a track, such as a local recorder, does not make the
track's bandwidth reservation demanded or the link probe treat the uplink as
watched; only readers across a connection do.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

A reservation is gated on `track::Demand`, which counts every reader,
including an in-process `subscribe` (`rs/moq-net/src/model/bandwidth.rs`,
tests around the demand gate). So a studio recording of its own broadcast
([moq-dev/studio#18](https://github.com/moq-dev/studio/pull/18)) reserves uplink and makes Health grade an
unwatched uplink as watched. Distinguish remote demand without making local
readers invisible to the encoder's own on-demand start.
