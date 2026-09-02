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

Add a generic send-group primitive to the selected QUIC core. A group owns its
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

If moq#3320 lands first, fold its proposed
`quest/m1/perf/stream-scheduler.md` into this quest and remove that index entry.
It describes the same scheduler outcome, not a second implementation.

Tests saturate the sender with differently sized audio and video groups and
prove byte fairness over a bounded window, strict preemption by a higher
priority, newest-first backlog shedding, ordered oldest-first delivery,
dynamic priority updates, blocked-stream handling, and cleanup on reset. Run
the same scenarios through raw QUIC and qmux.

## Required

- [Choose the parent and establish the fork](/quest/m2/quic/parent.md) - the
  scheduler must land against the selected protocol core

## Closes

- [#699](https://github.com/moq-dev/moq/issues/699) - close this issue when the
  quest finishes

## Related

- [moq#3320](https://github.com/moq-dev/moq/pull/3320) - removes the current
  dense-rank queue from the wide scalar path and records why a scalar cannot
  provide this fairness level
- [Transmission order](/quest/m1/ladder/transmit.md) - rendition priority is a
  policy consumer of the same hierarchy
