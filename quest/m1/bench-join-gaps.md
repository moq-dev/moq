# [S] moq-bench scores only gaps at or after a subscription's start

## Goal

`moq-bench`'s `groups_lost` counts only groups missing at or after the first
group a subscription receives. Older groups that arrive later, such as cached
history a relay fills in after the subscribe, are reported in their own
counter and never scored as loss. A test pins both cases.

## Plan

Today `GapTracker` (`rs/moq-bench/src/connection.rs`) lowers `min` to the
oldest sequence it sees. A history group arriving after the live frontier
therefore widens `groups_expected` back to that group, and every group
between it and the first live group counts as lost. A ramp of new
subscriptions then reads as a burst of loss even when no subscriber missed a
group it asked for.

Fix: pin the span's floor at the subscription's first received group, and
count earlier arrivals separately (for example `groups_history`) in
`bench.jsonl` and the live report. A group that explicitly fails stays lost
wherever it falls.

Found by a downstream perf gate: an announce profile's `groups_lost` rose
with each subscription ramp step and stopped once the ramp finished, while
the measured window had zero confirmed loss. The cause was over-fetched
history at join, not missing groups.
