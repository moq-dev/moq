# [S] Allocation-free single-range subscriber window

## Goal

Subscribing with one range, as nearly every subscriber does, resolves its
`Window` without a heap allocation, so a join costs what it did before
per-subscriber ranges ([#5929](https://github.com/moq-dev/moq/pull/5929)).

## Plan

The m0 performance audit measured `track_subscriber_join` with callgrind:
3,233 instructions per join before #5929, 6,950 after it, and 4,492 once the
audit's PR resolved the servable window once and kept a single run inline
(`Servable`). The remaining ~1,260 instructions are `Window::new` in
`rs/moq-net/src/model/subscription/window.rs` and its drop: it clones the
ranges into a `Vec`, pushes the spans into another, and inserts the groups into
a `RangeInclusiveSet`, so a default subscription allocates three times.

Give the common case an inline representation: one range, one span, and its
groups as a single run, spilling to the current collections only for several
ranges. This is private to moq-net. Measure instructions per join the way the
audit did (a loop of `subscribe` plus `poll_subscription_changed`, the
difference of two iteration counts under callgrind), and keep
`track_subscriber_join` and `track_subscriber_churn` as the Nightly check.

## Related

- [Performance audit](/quest/m1/audit-perf.md) - the recurring audit that found the cost
