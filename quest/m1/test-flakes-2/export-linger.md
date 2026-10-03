# [XS] Export linger test times from stdin close

## Goal

`a_clean_finish_exits_zero_once_the_linger_expires` in
`rs/moq-cli/tests/export.rs` stops failing its `>= 900ms` lower bound under
load, while still proving the export waits out its linger.

## Plan

The test starts its timer after the publisher process exits, but the export's
linger starts when the broadcast end reaches it, which can come first. Start
the timer before `drop(stdin)` instead: the broadcast can only end after stdin
closes, so that instant is a correct lower bound. Subprocess test, so a paused
clock can't apply.
