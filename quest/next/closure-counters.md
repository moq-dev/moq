# [S] Keep aggregate closure counters monotonic

## Goal

`moq_stats::aggregate` has one stated, tested rule for what a consumer reads
when a departed node returns: a same-lifetime rejoin never regresses a closure
counter, and a genuine restart is either floored to the last value or an
explicit fresh segment. The rule is documented as a consumer-facing contract.

## Plan

The stickiness fix ([moq#3625](https://github.com/moq-dev/moq/pull/3625))
keeps a departed node's last `Traffic` contribution and drops `Presence`
immediately. Its review raised an open P2: retiring the gauges advances the
open/closed pairs, so the next raw frame from the same node can show a lower
`*_closed` value and regress a monotonic reading.

Two directions were discussed, without a decision:

- Retire gauges on departure (current), accepting a closure-counter regression
  on rejoin.
- Keep the last gauges as a floor, which suppresses a still-live node's gauges
  after a transient reader failure.

Settle which counters must be monotonic and over what lifetime, and whether the
fresh-segment contract already permits a per-node closure reset or only a
cumulative traffic regression. The answer is a consumer-facing contract, so
document it in `moq-stats` rather than leaving it as an aggregate detail.

## Related

- [Client stats](/quest/next/qos/stats/README.md) - publishes the same counters
  to consumers
