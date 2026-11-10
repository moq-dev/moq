# [M] A padded TS export sheds when its sink falls behind

## Goal

A `moq export ts` with a mux rate (constant bitrate padded with null packets)
never writes a burst of back-filled slots after its sink blocks, and never
blames the source's clock for a sink slower than the mux rate. Like the
unpadded path, once it falls more than the budget (half the delay) behind, it
sheds: it skips the stale slots and resumes the PCR grid at the current time,
flagging the jump as a PCR discontinuity.

## Plan

Planned 2026-11-09 (maintainer, /quest-import of
[moq-dev/moq#8383](https://github.com/moq-dev/moq/issues/8383)). Milestone:
✅ m1 (rejected: m3, m4), since it is an additive fix to shipped code.

The report, verified against the code: the slot grid follows the jitter
clock, and `lay_due` (`rs/moq-mux/src/container/ts/export.rs`) lays every
slot already due in one pass. The CLI awaits `stdout.write_all` between
`next()` calls, so a blocked stdout stops polling and the resume lays out
every missed slot at once (75 MB of nulls after a 60 s block). Under a slow
reader, a non-empty `program.queue` returns before the jitter buffer
releases. The buffer fills, the tracks stop reading, and `steer`
(`rs/moq-mux/src/jitter.rs`) reads the shrinking slack as a source clock
about -10^6 ppm off and exits.

Decided 2026-11-09 (maintainer): ✅ shed like the unpadded path (rejected:
re-anchor after a block and error on a sustained slow sink; won't fix). A
single mechanism covers both cases: a 60 s block gives one flagged jump, and
a sink slower than the mux rate gives periodic jumps.

Guidance: skip the stale slots instead of generating them, through the
existing PCR discontinuity flag. Re-anchor the jitter clock along with the
grid, or `steer` still counts the output's own lag against the source. A
re-anchor is a clock jump, so it must set the flag
([moq-dev/moq#7981](https://github.com/moq-dev/moq/pull/7981) flags only
real jumps). Test on mock time: a sink blocked past the budget resumes with
one flagged discontinuity and no backlog, a sink at half the mux rate keeps a
bounded lag, and neither trips the drift error.

Public API: none expected. Wire: none.

## Closes

- [#8383](https://github.com/moq-dev/moq/issues/8383) - a padded export back-fills every missed slot after stdout blocks
