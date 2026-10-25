# [XS] The moq-mux missing-sequence test on a mocked clock

## Goal

`missing_sequence_skips_on_live_track`
(`rs/moq-mux/src/container/consumer.rs:2537`) never fails under load.

## Plan

It wraps the read in a real `tokio::time::timeout(1s)` while the skip waits
on the consumer's `hole_timer` (`moq_clock::sleep_until`, 100 ms
`max_delay`) on wall time. Pause the clock the way its sibling
`gap_keeps_late_arriving_group_within_max_delay` does.

Public API: none. Wire: none.
