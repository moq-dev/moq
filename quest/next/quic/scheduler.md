# [XL] Add hierarchical QUIC stream scheduling

## Goal

One QUIC connection expresses the MoQ scheduling hierarchy without packing it
into a scalar: higher-priority subscriptions preempt lower-priority ones,
backlogged subscriptions at the same priority receive equal bandwidth within
their scheduling domain, and each subscription serves its own group streams
newest-first.

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
sizes. Within the chosen group, order streams by the MoQ group order: newest
first, fixed by the draft and never inverted. A blocked stream must not consume
the group's turn, and opening newer groups must not reset its accumulated
fair-share credit.

Map conventions only at adapters. MoQ's model remains higher value first, the
IETF wire remains lower value first, and browser `sendOrder` remains local to
its WebTransport send group. Native QUIC and qmux use the full three levels;
a browser that cannot prioritize send groups gets the lower two levels without
pretending to provide strict subscription priority.

Give every MoQ subscription one send group. A SUBSCRIBE_UPDATE changes the
group priority atomically. Group streams use their position within the
subscription, never another subscription's sequence. Remove the session-wide
`lite::PriorityQueue` once every enabled backend has an honest implementation
or fallback.

Add the scheduling-domain tier above subscriptions: on a shared session,
fairness is one deficit-round-robin bucket per domain, and a domain's
subscriptions share that bucket by their own priorities, so a tenant opening
more subscriptions never buys more bandwidth. The domain key and its scope are
[Scope track priority](/quest/next/track-priority-scope.md)'s; this quest owns
the tier and the weighting.

### Prototype and compare the hierarchy

The delivered API is hierarchical send groups. Do not require a separately
published scalar-widening API or drop fairness as an intermediate release.
Prototype the byte-accounted scheduler in the fork. An existing Quinn prototype can
supply a workload baseline, but a recency field alone does not prove byte
fairness: differently sized writes must spend the group's byte credit, and
requeueing or adding streams must not reset that credit.

Compare the hierarchy with the existing MoQ queue and two private benchmark
baselines: a scalar `[priority][group sequence]`, and a scalar containing only
priority. The first removes queue coordination but lets unrelated sequence
numbers compete across subscriptions; the second exposes per-stream fairness
but loses newest-first ordering within a subscription. Neither is the public
API this quest ships.

Keep limitations explicit in the scalar benchmark. A 32-bit key leaves only
24 bits after track priority, so wraparound and sparse sequence values can
invert group order. Browser numeric precision and narrow backend urgency
fields impose different limits. Include these cases and compare audio/video
subscriptions with different group cadences; do not treat scalar ordering as
an exact equivalent of the hierarchy.

Record queue/lock work, CPU, throughput, latency, and byte fairness on the
same congested workloads. Both equal-priority subscriptions must progress
while a higher-priority subscription preempts them and each subscription
sheds its own old backlog. Measure the full scope of trait and adapter changes
before publishing the API; any published break targets dev under the normal
release policy.

Retransmissions follow the same hierarchy. noq already re-queues a lost
range through the stream's priority (`StreamsState::retransmit`), so a lost
video range waits behind new audio at a higher priority; keep that, and add
a test proving it, plus one proving a retransmission is never starved
indefinitely by an equal-priority group's new data (the fair tier's byte
credit covers retransmits too).

Tests saturate the sender with differently sized audio and video groups and
prove byte fairness over a bounded window, strict preemption by a higher
priority, newest-first backlog shedding, dynamic priority updates,
blocked-stream handling, sequence wrap and sparse sequence values, and cleanup
on reset. This quest owns native QUIC proof and
reusable scheduling fixtures. [qmux](/quest/next/quic/qmux.md) owns running those
fixtures through its record writer after adopting the scheduler; native
scheduler completion must not wait for that dependent integration. Preserve
working behavior on backends not yet migrated, and remove queue code only
where the new implementation makes it redundant.

## Required

- [Fork noq](/quest/next/quic/fork.md) - the scheduler lives there

## Closes

- [#699](https://github.com/moq-dev/moq/issues/699) - close this issue when the
  quest finishes

## Related

- [moq#3320](https://github.com/moq-dev/moq/pull/3320) - removes the current
  dense-rank queue from the wide scalar path and records why a scalar cannot
  provide this fairness level
- [Ladder controller](/quest/next/ladder/controller.md) - rendition priority is
  a policy consumer of the same hierarchy
- [Scope track priority](/quest/next/track-priority-scope.md) - owns the
  priority semantics this mechanism realizes, including the scheduling-domain
  scope
