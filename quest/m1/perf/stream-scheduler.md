# [L] Prototype a three-level stream scheduler in our own QUIC stack

## Goal

MoQ wants three scheduling levels: strict by track priority, round-robin between
subscriptions at equal track priority, then strict newest-group-first within a
subscription. Nothing available offers all three, and a scalar send order cannot
express the middle one at all, since any total order picks a winner and that
winner takes strict precedence.

Prototype the real shape in a QUIC stack we own, where the send loop is ours,
rather than negotiating it into someone else's scheduler first.

## Plan

What exists today, which is more than it looks:

- quinn is already strict-priority then round-robin. `PendingStream` is ordered
  `(priority, recency, id)` in a max-heap, where `recency` is a monotonically
  decreasing counter that requeues a stream behind its equal-priority peers once
  it writes, and `TransportConfig::send_fairness` defaults to true. That is
  levels one and two.
- quiche is the same two levels: `urgency` strict, `incremental` round-robin
  within an urgency.
- W3C [`sendGroup`](https://www.w3.org/TR/webtransport/#sendGroup) is levels two
  and three, the other pair: equal allocation between send groups, strict
  `sendOrder` within one. Send groups carry no priority relative to each other,
  so it cannot also carry track priority.

So both native backends already give the top two levels, `sendGroup` gives the
bottom two, and nothing gives three. MoQ currently sees none of the round-robin
because it hands every stream a distinct priority, which means no two streams are
ever equal and the fairness never engages. We pay for a queue to defeat a
scheduler already doing half the job.

The missing piece is one field, not a scheduler. quinn's ordering key wants to
become `(priority, bucket_recency, order, id)`: `priority` strict for track,
buckets round-robin against each other reusing the existing recency counter,
`order` strict within a bucket for group sequence. That is `sendGroup` semantics
plus the priority dimension `sendGroup` lacks.

Prototype it in moq-uring's own stack rather than as a quinn fork. We own that
send loop, the ordering key is ours to shape, and it avoids carrying a fork
before the semantics are settled. If it works, that is the evidence for an
upstream quinn change or a spec issue, and the fallback stays a fork.

Measure against the two points reachable without any of this, both of which are
real options rather than strawmen: a scalar send order of `[track][group]`, which
buys strict priority and newest-first while giving up fairness (see [Send order
width](/quest/m1/perf/send-order-width.md)), and a scalar of `track` alone, which
lets quinn's existing fairness through and gives up newest-first instead.

Acceptance: a congested session carrying two equal-priority tracks of different
group cadence, audio against video, keeps both progressing rather than draining
one, while a higher-priority track still preempts both and newest-first still
sheds backlog within a track. Compare all three configurations on the same shape.

## Required

- [Send order width](/quest/m1/perf/send-order-width.md) - the scalar lands first

## Related

- [Priority set_track wakes](/quest/m1/perf/priority-set-track-wakes.md) - dead once the queue goes
