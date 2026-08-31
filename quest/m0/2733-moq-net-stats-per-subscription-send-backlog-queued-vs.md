# [M] moq-net stats: per-subscription send backlog (queued vs delivered) + skip counters

## Goal

Implement and verify the behavior tracked in [#2733](https://github.com/moq-dev/moq/issues/2733)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

### moq-net stats: per-subscription send backlog (queued vs delivered) + skip counters

Design context: moq-dev/moq.pro#992 (per-broadcast QoS on the CDN dashboard), but the metric is generally useful to any moq-net consumer.

#### The metric

The strongest media-agnostic congestion signal a relay/publisher has is *queued outstanding data per subscription*: bytes produced for a subscriber that haven't been delivered yet. Defined per **active subscription**, so an unrequested track has no row - 0 Mb/s from lack of demand never reads as congestion.

With `latency_max`, the queue is bounded and overflow becomes group skips. So congestion is a pair:

- **queue level** (gauge) - pressure building; keyframe-burst spikes are normal, sustained level means the peer is slower than the media
- **skips** (cumulative) - pressure that already cost quality

#### Proposed `Traffic` additions

`Traffic` is `#[non_exhaustive]` with `#[serde(default)]` everywhere, so these are additive. Per the existing `(tier, broadcast, role)` bucketing, the `role=Subscriber` rows aggregate over all of that broadcast's subscriptions on the node:

- `queued_bytes` (gauge): sum over active subscriptions of produced-but-undelivered bytes
- `queued_max_bytes` (gauge): worst single subscription, so one dying viewer isn't averaged away by fifty healthy ones
- `subscriptions_congested` (gauge): count of subscriptions over a backlog threshold - "how many viewers are hurting" without per-viewer tracks
- `skipped_bytes` / `skipped_groups` (cumulative): dropped due to the latency budget

Gauges fit the shared-atomics `Meter` design: increment on produce, decrement on delivery/skip, O(1) snapshot. One correctness detail: a skipped/aborted group's bytes must leave the queue gauge (they were never delivered, but they're no longer queued either), or the gauge leaks upward on every skip.

#### What "delivered" means, in two parts

```
total backlog = moq-layer lag    (produced into group cache - accepted by QUIC)
              + QUIC-layer unacked (accepted by QUIC - acked by peer)
```

- The moq-layer term is measurable now at the model/wire seam: the model knows the producer offset, the session's writer task knows how far it has flushed into the stream.
- The QUIC-layer term needs per-stream delivered-vs-queued from the transport, which nothing exposes publicly today (quinn/quinn-proto/quiche have the state internally; browser `WebTransportSendStream.getStats()` is spec'd but unimplemented - verified absent in Chrome 148). Tracked in moq-dev/web-transport#368. This term matters: QUIC accepts writes up to the peer's flow-control credit regardless of congestion, so with a generous receiver window most of the backlog sits inside the QUIC library, invisible at the moq seam.

Ship the moq-layer term first with the gauge semantics designed so the QUIC term can be folded in per-backend as it becomes available (fields are already `Option`-shaped at the trait level; absence must read as "unknown", not zero-and-healthy... concretely: keep the moq-layer gauge always-on, and consider a separate field or a capability note for whether the QUIC term is included, so a consumer comparing two nodes doesn't compare different definitions).

Because moq maps one group to one QUIC stream, every stream belongs to exactly one subscription - per-subscription attribution is structural.

#### Publisher side

The same gauge on the `role=Publisher` direction (bytes produced locally vs accepted/delivered toward the relay) is the publisher's uplink-health signal - "media bitrate exceeds what the network is draining" - and complements PROBE's peer-measured receive rate.

## Closes

- [#2733](https://github.com/moq-dev/moq/issues/2733) - close this issue when the quest finishes
