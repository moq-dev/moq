# [M] A time end between two groups ends the read at the next group

## Goal

A bounded read whose time end falls between two held groups ends as soon as the next held group starts past that end. It does not wait out `delay.max` for a straggler. This is the migration form of a former exclusive end, `Time(t - 1)`, so binding users who migrated hit it. The behavior matches between Rust and `@moq/net`, for start-order and arrival-order reads.

## Plan

Found by [moq-dev/moq#9190](https://github.com/moq-dev/moq/pull/9190). On a live, locally produced track with groups at 1000, 2000 and 3000, an ordered read of `Time(1500)..=Time(1999)` hands out 1000 and then parks until `delay.max` ages out keys 1001..=1999, or forever with no max. `Time(1500)..=Time(2000)` ends at once: handing out 2000 passes every key below it. The same read in arrival order waits the budget out even with the end at 2000. The old exclusive `Absolute` end in the bindings lowered to the same `Time(t - 1)`, so this is not a regression.

The wait comes from `Cursor::is_exhausted` in `rs/moq-net/src/model/track.rs`. It requires every key in the window up to the end to be delivered, written off, given up, dropped, or skipped. A local producer that creates group 2000 after 1000 records no skip of 1001..=1999. A lite-07 copy does learn that skip from the GROUP's Previous Start.

Recommendation: an ordered read ends once the next held group lies past every range, since it would have handed that group out and passed the keys below it anyway. An arrival-order read keeps waiting for a straggler that a local producer may still create below the end, unless the producer records the skip. Weigh that against having `create_group` above the highest key record the keys between as skipped, which gives a local track the same knowledge a lite-07 copy has. Check JS for the same behavior and fix both together.

Repro, as a test beside `a_range_reads_only_its_groups`: cache groups 1000, 2000, and 3000 with one frame each, subscribe with `replay().with_order(Order::Ascending).with_ranges([Time(1_500)..=Time(1_999)])`, call `.ordered()`, and poll `poll_next_group`. It returns 1000 and then stays pending instead of ending.
