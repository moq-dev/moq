# [S] moq-c's raw track options test reads its first group under load

## Goal

`rs/moq-c/src/test.rs::raw_track_subscription_options_and_update` passes
every run of the full moq-c suite.

## Plan

Found 2026-11-16 by `just check` on
[moq-dev/moq#8958](https://github.com/moq-dev/moq/pull/8958), whose change
does not reach this test's Absolute ranges. Run alone it passes. Running the
whole `moq-c` unit binary (`test::` filter) 20 times in a row fails it 6
times at the base commit 949406e072: at `rs/moq-c/src/test.rs:2794`, it
receives `two` where it expects `one`, the group starting at 20 ms.

The test publishes three groups 20 ms apart, then subscribes with a 25 ms
`max_us` and two ranges: `[20000, 20001)`, holding only group `one`, and an
open one from 100,000,000 µs. Group `two` (40,000 µs) is in neither, so a
stale skip of `one` under load would explain the read missing `one` but not
receiving `two`: that read should then wait on the open range. Trace how the
consumer hands out `two` first. If a path admits a group outside the
requested ranges (such as a read before the ranges resolve, a drop or skip
that widens the range, or moq-c's own frame pump), that is a range-delivery
bug in the library, and the fix belongs there with a regression test. Only
if the trace shows `two` arrives by a path the ranges allow is the fix in
the test, as the epic's rules ask: drive it on a paused or manual clock, or
give it a budget that does not depend on how fast the suite runs.

Public API: none. Wire: none.
