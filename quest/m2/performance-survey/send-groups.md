# [S] QUIC send-group capacity after subscription churn

## Goal

A measured verdict on the memory that moq-quic's scheduler keeps in its
send-group `Slab` after a connection's concurrent subscriptions peak and
fall. Either the slab gives the memory back, or the quest records why the
retained bytes per peak subscription are acceptable.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7740](https://github.com/moq-dev/moq/pull/7740)): milestone m2,
in the performance survey, with the same "measure before reclaiming" policy
as [task capacity](/quest/m2/performance-survey/task-capacity.md).

- `rs/moq-quic/src/connection/streams/schedule.rs`: `groups:
  Slab<Group<K, D>>` (around line 177) grows in `open_group`, and
  `free_group` only removes the entry. Each `Group` holds a `Queue` of
  BTreeMaps, priority, deficit, ring, and members. Explicit send groups are
  opened per subscription (`rs/moq-net/src/lite/publisher.rs`
  `session.send_group(...)`, and the same in `ietf/publisher.rs`), so a
  relay connection keeps slots for its peak concurrent subscriptions.
- Measure with #7740's heap-delta approach (its `receive_bursts`
  benchmark): open N subscriptions on one connection, close them, and record
  retained bytes per peak subscription and how long they are held.
- If it matters at relay scale, shrink on a low-water mark, or reuse freed
  slots across groups so idle entries hold no queue allocation. Otherwise
  record the numbers and close.

Public API: none. Wire: none.
