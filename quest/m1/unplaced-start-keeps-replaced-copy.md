# [M] A front's reader keeps a replaced copy while a time start it may place is unplaced

## Goal

A front's reader whose range has a time start no copy has placed yet keeps a
replaced copy that may still land the group holding that time, so the read
places the start and ends as a plain cursor on the logical track would. Today
it lets that copy go and the read stays pending for good, in Rust
(`rs/moq-net/src/model/resume.rs`) and, if it fails the same way, JS
(`js/net/src/resume.ts`).

## Plan

Found on [moq-dev/moq#9401](https://github.com/moq-dev/moq/pull/9401), on
`main` with that PR's fixes applied. Reproduction, as a `resume.rs` unit test:
a front serves received, anchored copy `b`, a reader subscribes `7..=7`, `b`
writes group 8, then the front serves `c` (`start_at(9)`, writes 9), then `b`
writes 6 and 7. The reader reads nothing, `resolved_range(0)` stays `None`,
and `poll_recv_group` stays pending. A reader that had read group 8 from an
earlier copy before the range moved places it at 7 and ends, so only a reader
whose window is still unplaced when the switch caps `b` fails.

`Copy::retains` waits on a replaced copy only for the runs of the reader's
window (`reader.window.runs(...)`), and an unplaced time start has none, so
`b`, capped at 8, is `Retains::No` before group 7 lands. The backfill the
switch asks of `c` from the time is let go too, since `c`'s feed starts at 9.
Wait on a replaced copy while an unplaced range's time could still land under
its cap: from the range's time up to the cap, as `may_land` judges it.

Once fixed, a front served by another front should pass the same shape: the
inner front is such a reader for the outer front's backfill. Add both tests,
and their JS mirrors.
