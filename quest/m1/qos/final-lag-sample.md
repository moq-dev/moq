# [XS] A closing subscription takes its last lag sample

## Goal

When an egress subscription ends, the bytes its track produced since the last
stats tick still land in the `lag` histogram at its final lag, instead of
being lost. A subscription that opens and closes between two ticks is sampled
at least once.

## Plan

Lag is sampled only on `Registry::report` ticks: `Counters::sample` in
`rs/moq-net/src/stats.rs` walks weak `FrontierInner` references and prunes
the dead ones without sampling them, so a subscription's last partial
interval disappears with it.

- The subscription guard and each in-flight group `Delivery` hold a strong
  reference, so the frontier dies only once the last of them does. Its drop is
  the natural place for one final `sample(now)` into the same counters.
- No double counting: `sample` advances each source's `sampled` bytes under the
  frontier lock, so a tick racing the drop leaves nothing for it to count again.
- Test: a subscription that opens and closes between ticks while its track
  produces lands those bytes in the histogram; one that closes mid-interval
  adds exactly the bytes since its last sample. Note the behaviour in
  `doc/concept/stats.md`.

Public API: none. Wire: none, only the histogram's values change.

## Required

- [Starvation](/quest/m1/qos/starvation.md) - the sampler this extends (#4298)
