# [M] Divide the connection bandwidth estimate among concurrent encoders

## Goal

Implement and verify the behavior tracked in [#2815](https://github.com/moq-dev/moq/issues/2815)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Found during the adversarial review of #2809.

#### Problem

`moq_net::bandwidth::Consumer` carries a **per-connection** send-bandwidth estimate, but rate control is applied **per-encoder**: `rate::Policy::headroom` (default `0.9`) makes each encoder target 90% of whatever estimate it is handed, and `rate.rs`'s own doctest shows a 2 Mbps estimate producing a 1.8 Mbps target.

Nothing divides that estimate when several encoders share one connection. Two capture pipelines on the same session each target ~90% of the same uplink, or ~180% in aggregate, plus audio and transport overhead. The result is sustained queueing exactly when adaptive rate control should be preventing it.

The `headroom` doc even states the assumption: it reserves room "for the other tracks sharing this connection (audio)", i.e. one video encoder per connection.

#### Current mitigation

\#2809 lets one `moq` process run several import stages, which is what makes multiple capture encoders on one connection reachable. It refuses the combination rather than oversubscribing:

```
only one stage can encode to fit the connection's bandwidth estimate, but 2 do;
run them as separate processes, or publish over --server-bind, which has no estimate
```

Separate processes are unaffected: each gets its own connection and its own estimate. (Strictly, they then oversubscribe the *physical* uplink instead, which is the same underlying gap seen from one layer up.)

#### What a fix needs

Allocation across the encoders sharing a connection, so their targets sum to the estimate rather than each matching it. Roughly:

- a way to split one `bandwidth::Consumer` into N shares (equal, or weighted by each encoder's configured ceiling), or
- a `policy` knob on `moq_video::encode::Options` so a caller can set `headroom` itself, with moq-cli dividing by the number of adaptive stages.

The first is better: weighting by ceiling handles a 1080p and a 360p rung sharing a link far better than an even split, and it keeps the policy in one place rather than making every caller reinvent it.

Lifting the refusal in moq-cli, plus a regression test covering two capture stages, is the acceptance criterion.

## Closes

- [#2815](https://github.com/moq-dev/moq/issues/2815) - close this issue when the quest finishes

## Related

- [#2859: Passthrough imports reserve no bandwidth, so a co-resident encoder…](/quest/m0/2859-passthrough-imports-reserve-no-bandwidth-so-a-co-resident.md) - related open work
- [#2848: Follow the bandwidth grant in moq-audio instead of holding a fixed…](/quest/m0/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - related open work
- [#2847: The quinn backend's send-bandwidth estimate is cwnd/rtt, not a rate](/quest/m0/2847-the-quinn-backends-send-bandwidth-estimate-is-cwnd-rtt.md) - related open work
