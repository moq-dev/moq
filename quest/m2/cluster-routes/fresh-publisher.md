# [L] Model and measure a fresh publisher's doubt exemption

## Goal

Determine whether a fresh authenticated direct publisher session can recover
its own route before the one-second doubt expires, without admitting a stale
return or a request loop. Compare it with timer-only doubt before deciding
whether to adopt it.

## Plan

Decided 2026-11-13 (coordinator, `/quest-afk`, while the maintainer was away):
timer-only doubt is the baseline in
[moq-dev/moq#8622](https://github.com/moq-dev/moq/pull/8622), and this exemption
is modeled and measured separately. Early collection is rejected because it
permits the request loop recorded by `Loop.cfg`. These choices remain open
to change in review.

The fixed 24-relay reconnect cases take 1,050 ms in a clique and 1,875 ms in
a ring, including propagation at 25 ms per hop, versus 50 ms and 325 ms for
the distance rule. Both path policies wait because heard routes still name
the doubted publisher. The exemption could remove that wait for a publisher
returning directly, but not for a relay presenting somebody else's route.

Model what authenticates the publisher and binds the offer to the new direct
session, including delayed offers from the old session, reconnects, and
concurrent link failures. Keep the timer-only model as the control. Reproduce
counterexamples on the router and measure the same reconnect and churn
scripts on both policies, reporting traffic, request recovery, stale routes,
and reachable but unrouted observations. Preserve the baseline's safety
checks; a faster reconnect alone does not justify the exemption.

Recommend limiting any exemption to the new authenticated publisher's own
direct offer. A forwarded offer or a new transport session alone does not
establish that the origin is alive. Record the evidence and adoption decision
before changing the default, and mirror an adopted rule in Rust, JS, and the
draft wherever the wire switch has already landed.

## Waiting

- Maintainer: after the model and measurements, adopt the narrowly authenticated
  direct-publisher exemption or retain timer-only doubt. Recommend adoption only
  if it preserves the safety checks and improves the measured reconnect cases;
  otherwise retain the baseline. An agent can gather that evidence, but adopting
  an exception changes the coordinator's accepted routing policy and needs an
  explicit decision.

## Related

- [lite-07 sessions run the path router](/quest/m2/cluster-routes/switch.md) - supplies the authenticated session boundary when it puts the rule on the wire
