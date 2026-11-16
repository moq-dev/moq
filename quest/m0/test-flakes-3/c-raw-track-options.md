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
`max_us` and a range starting at 20 ms. Under load, the read likely comes
after group `one` went stale by its successor's arrival on the host clock, so
the read skips it. That is correct behaviour for a 25 ms budget, so the fix is
in the test, as the epic's rules ask: drive it on a paused or manual clock,
or give it a budget that does not depend on how fast the suite runs. Confirm
the cause before fixing.

Public API: none. Wire: none.
