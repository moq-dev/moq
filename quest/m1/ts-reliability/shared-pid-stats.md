# [M] TS stats count a shared PID once

## Goal

`ts::stats` counts each PID once, in the parser every program shares, so a
PID two programs carry has one continuous set of TR 101 290 counters across
program starts and retirements. The multiplex totals read those counters
directly instead of rebuilding them from per-program copies.

## Plan

Found while landing [moq-dev/moq#6689](https://github.com/moq-dev/moq/pull/6689):

- Each program keeps its own counters for the PIDs it reads, and the
  multiplex totals merge them, then subtract what a retiring program still
  shares with a running one. Most of that PR's review churn came from this
  merge-and-subtract logic.
- A program that starts after another on a shared PID retired counts that
  PID's timing checks only from its own start. Where both run at once, the
  larger count stands.

Decided 2026-10-28 (maintainer): one quest, since counting in the shared
parser both closes the timing gap and deletes the merge-and-subtract logic.
Keep #6689's outcomes: a retired program's final picture still counts, a
missing PMT still refuses after 0.5 s of program clock, and an unfinished
section on a PMT PID gets its bad CRC counted once. Extend the `ts_programs`
bench if the shared counters change its cost.

Rust: `rs/moq-mux` `container/ts/stats.rs`, `programs.rs`, `unclaimed.rs`.

Public API: none expected; if `ts::stats`' per-program view changes shape,
say how to migrate. Wire: none.
