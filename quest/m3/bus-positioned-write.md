# [M] Bus writes land at a bus position

## Goal

A `bus::Channel` write can say which bus position its samples belong at, and
the bus places them there, so a caller lines sound up with video without
copying the bus's private latency and padding rules.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

- ✅ **Positioned writes**, a breaking change to `Channel::write`.
  Rejected: exposing a channel's buffered depth and latency, which leaves the
  caller doing the placement and keeps the dry-channel error below.

Today `Channel::write` (`rs/moq-audio/src/bus.rs`) takes no position, and
`LATENCY` (50 ms) and `HEADROOM` are private; positions exist only on the
output side (`Block::position`, `Driver::position`). Studio broadcast sync
([moq-dev/studio#38](https://github.com/moq-dev/studio/pull/38)) copies those constants, and sound can trail
video by up to 37.5 ms after a channel runs dry. Decide what a write for a
position already mixed or beyond the headroom does, and fail loudly rather
than silently shifting it.
