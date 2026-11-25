# [S] A JS front's ordered read returns a gap held across a switch

## Goal

A JS ordered reader on a front returns the gap its serving copy reported and it held behind a lower group still owed, once that group lands, after the route switches. The Rust front already does. `js/net/src/resume.ts` and `js/net/src/track.ts`.

## Plan

Preserved from closed [moq-dev/moq#9437](https://github.com/moq-dev/moq/pull/9437); it fails on `main` even without widening. A received copy `a` holds groups 998 and 2000 and gave up `1000..1500` stale under 10s. An ordered read of `998..1500` at a 1s budget asks a fill, which refuses the run as old, and the gap waits behind group 999. A new route `b` then serves and drops `1000..1500` as not found. Once group 999 lands on `a`, the Rust front returns `[group 999, gap 1000..1500]`, while the JS front returns only `[group 999]` and waits forever. Without the switch, JS returns the gap too.

Reproduce by porting `a_replaced_copys_stale_gap_stands_once_the_read_widens` from `rs/moq-net/src/model/resume.rs` (see #9437's diff for the test shape) to `js/net/src/resume.test.ts`, with and without its widening. Find where the switch loses the gap (in the reader's `#drops`, the logical record's `#reported`, or the ordered read's gaps), fix it, and add the regression test.

Public API: none. Wire: none.

## Related

- [A widened resumed ordered reader asks its copies' stale give-ups again](/quest/m3/widened-front-reader-asks-stale-again.md) - widening half moved to m3 behind judged OLD drops
