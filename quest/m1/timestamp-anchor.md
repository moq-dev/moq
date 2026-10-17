# [XS] One process-wide Timestamp::now() anchor

## Goal

`Timestamp::now()` counts from one process-wide anchor, so a test that mixes
a paused runtime with the FFI runtime sees catalog stamps that agree instead
of going backwards and tripping the rewind check from #5803. A regression
test mixes the two and fails without the fix.

## Plan

Decided with the maintainer on 2026-10-17: an XS quest in m1, from #5803,
which worked around this by running moq-ffi's
`catalog_now_fixes_the_clock_for_later_media` (`rs/moq-ffi/src/test.rs`)
unpaused. Put that test back on a paused runtime as the regression.

Where it lives: `anchor()` in `rs/moq-net/src/model/clock.rs` keys an offset
on each ambient clock's `moq_clock::anchor()` and draws a fresh random
jitter (up to 69 s) for each key. A paused tokio runtime whose clock starts
before the process origin gets an anchor of its own (`paused::anchor` in
`rs/moq-clock/src/runtime.rs`), because its instants share no scale with the
process-wide `ANCHOR`. So the two runtimes map `now()` through different
anchors and different jitters. Per-clock anchors are deliberate:
`each_ambient_clock_owns_its_wall_anchor` pins them for the simulator, so
propose in the PR how paused runtimes join the process-wide anchor without
breaking that.
