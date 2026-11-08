# [M] Bus writes land at a bus position

## Goal

A `bus::Channel` write can say which bus position its samples belong at, and
the bus places them there, so a caller lines sound up with video without
copying the bus's private latency and padding rules.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent. Moved to m2: Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests). `moq_audio::bus` is new since `release`, so the cut publishes `Channel::write` for the first time, and this break after it would cost another moq-audio major. If the positioned-write semantics can't settle in time, land `write(Write { position: Option<u64>, data })` so positioning is additive.

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

[moq-dev/studio#93](https://github.com/moq-dev/studio/pull/93) adds a second
case: after a silence the studio's `playout::Writer` can't tell how much its
channel holds, so a first write lands up to one block (10 ms) early. Broadcast
and media sources both hit it. A positioned write fixes this too; test that a
first write after silence is mixed at the sample it was meant for.

- ✅ **Fold studio#93 in here**, decided 2026-11-07 (maintainer, /quest-complete).
  Rejected: an interim bus-position accessor quest, and dropping the plan.
