# [L] Add hierarchical QUIC stream scheduling

## Goal

One QUIC connection expresses the MoQ scheduling hierarchy without packing it
into a scalar: higher-priority subscriptions preempt lower-priority ones,
backlogged subscriptions at the same priority receive equal bandwidth, and
each subscription chooses newest-first or oldest-first service among its own
group streams.

This completes [#699](https://github.com/moq-dev/moq/issues/699). In the
reported Alice and Bob case, two priority-4 subscriptions continue to make
progress at the same byte rate even when one has a deeper or faster-growing
group backlog.

## Plan

Add a generic send-group primitive to noq-proto. A group owns its
strict priority and fair-share scheduler state; each stream owns only its
order within that group. Prefer an owned group handle whose drop removes its
scheduler state and whose priority can be updated without walking every open
stream. If the backend-neutral WebTransport trait cannot carry a handle
without breaking object safety, allocate an opaque group ID from the session.
Do not accept caller-chosen IDs and do not compress the hierarchy into another
integer.

Use byte-accounted deficit round robin, or an equivalent bounded-quantum
algorithm, between backlogged groups at equal priority. Round robin by stream
count is insufficient because audio, video, and data streams have different
sizes. Within the chosen group, order streams strictly by the MoQ group order:
newest first by default, oldest first for an ordered subscription. A blocked
stream must not consume the group's turn, and opening newer groups must not
reset its accumulated fair-share credit.

Map conventions only at adapters. MoQ's model remains higher value first, the
IETF wire remains lower value first, and browser `sendOrder` remains local to
its WebTransport send group. Native QUIC and qmux use the full three levels;
a browser that cannot prioritize send groups gets the lower two levels without
pretending to provide strict subscription priority.

Give every MoQ subscription one send group. A SUBSCRIBE_UPDATE changes the
group priority atomically. Group streams use their sequence position and the
subscription's `ordered` setting, never another subscription's sequence.
Remove the session-wide `lite::PriorityQueue` once every enabled backend has
an honest implementation or fallback.

### Prototype first, as a quinn patch

Nothing available offers all three levels, but quinn already has two of them
and the counter the third needs: `PendingStream` is ordered
`(priority, recency, id)` in a max-heap, where `recency` is a monotonically
decreasing counter that requeues a stream behind its equal-priority peers once
it writes, and `TransportConfig::send_fairness` defaults to true. quiche's
`urgency` plus `incremental` is the same pair, and W3C `sendGroup` is the other
pair (fair between groups, strict `sendOrder` within one, no priority between
groups). MoQ sees none of quinn's fairness today because it hands every stream
a distinct priority, so no two streams are ever equal.

The missing piece is one field: the ordering key becomes
`(priority, bucket_recency, order, id)`, with `priority` strict for the track,
buckets round-robin against each other on the existing recency counter, and
`order` strict within a bucket for group sequence. That field lives in the
QUIC stack, and moq-uring does not change that: its quinn path hands stream
selection to `poll_transmit`, so what moq-uring owns is the UDP path, not the
key. Carry it as a quinn patch rather than a fork (one field in one struct
plus a `SendStream` setter), run it under moq-uring's quinn backend where the
relay workload is, and treat a working prototype as the evidence for the noq
proposal above. quiche stays out of scope: reproducing it there is a second
fork's worth of work and its top two levels already match.

Measure against the two configurations reachable without any of this, both
real options: a scalar send order of `[track][group]`, which buys strict
priority and newest-first while giving up fairness (see
[Send order width](/quest/m1/perf/send-order-width.md)), and a scalar of
`track` alone, which lets quinn's fairness through and gives up newest-first.
A congested session carrying two equal-priority tracks of different group
cadence, audio against video, must keep both progressing rather than draining
one, while a higher-priority track still preempts both and newest-first still
sheds backlog within a track. Compare all three on the same shape.
It describes the same scheduler outcome, not a second implementation.

Tests saturate the sender with differently sized audio and video groups and
prove byte fairness over a bounded window, strict preemption by a higher
priority, newest-first backlog shedding, ordered oldest-first delivery,
dynamic priority updates, blocked-stream handling, and cleanup on reset. Run
the same scenarios through raw QUIC and qmux.

## Required

- [Send order width](/quest/m1/perf/send-order-width.md) - the scalar lands first; the prototype is measured against it
- [Establish the noq relationship](/quest/m2/quic/parent.md) - the scheduler
  is proposed to noq first

## Closes

- [#699](https://github.com/moq-dev/moq/issues/699) - close this issue when the
  quest finishes

## Related

- [moq#3320](https://github.com/moq-dev/moq/pull/3320) - removes the current
  dense-rank queue from the wide scalar path and records why a scalar cannot
  provide this fairness level
- [Transmission order](/quest/m1/ladder/transmit.md) - rendition priority is a
  policy consumer of the same hierarchy
