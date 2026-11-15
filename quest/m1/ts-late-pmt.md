# [S] A late PMT ends only its own program

## Goal

Under `--program all`, a program the PAT lists whose PMT does not arrive is
reported as TR 101 290 `PMT_error_2` in the import's health, and every other
program keeps publishing; the late program publishes once its PMT arrives.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
`Programs` (`rs/moq-mux/src/container/ts/programs.rs`, from
[moq-dev/moq#6252](https://github.com/moq-dev/moq/pull/6252)) returns
`Err(self.late())` once any listed program waits more than 0.5 s of program
clock, so an MPTS whose PAT keeps listing a service after its encoder dies, or
announces one before its PMT starts, ends every program's broadcast. The
guide documents that ("A listed program whose PMT does not arrive within
0.5 s of program clock ends the import"), but its Loss section says feed
health "is logged and reported without changing what publishes". Treat the
late program's PIDs as unclaimed and update `doc/bin/ts.md`.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) report and continue (rejected: (b) keep ending the import and reword the Loss section).

Public API: none. Wire: none.

