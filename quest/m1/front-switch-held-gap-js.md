# [S] A JS front's ordered read returns a gap held across a switch

## Goal

A JS ordered reader on a front returns the gap its serving copy reported and it
held behind a lower group still owed, once that group lands, after the route
switches. The Rust front already does. `js/net/src/resume.ts` and
`js/net/src/track.ts`.

## Plan

Found on [moq-dev/moq#9437](https://github.com/moq-dev/moq/pull/9437); it fails
on `main` too. A received copy `a` holds groups 998 and 2000 and gave up
`1000..1500` stale under 10s. An ordered read of `998..1500` at a 1s budget asks
a fill, which refuses the run as old, and the gap waits behind group 999. A new
route `b` then serves and drops `1000..1500` as not found. Once group 999 lands
on `a`, the Rust front returns `[group 999, gap 1000..1500]`, the JS front only
`[group 999]`, and then waits for good. Without the switch, JS returns the gap
too.

Reproduce with the Rust `a_replaced_copys_stale_gap_stands_once_the_read_widens`
in `rs/moq-net/src/model/resume.rs` ported to `js/net/src/resume.test.ts`, with
and without its widening. Find where the switch loses the gap: in the reader's
`#drops`, the logical record's `#reported`, or the ordered read's gaps. Then
port that test.

## Related

- [Front ordered gap below its group](/quest/m1/front-ordered-gap-below-its-group.md) - other gap loss on a front's ordered read
