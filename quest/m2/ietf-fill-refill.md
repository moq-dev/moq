# [M] A moq-transport group head fills its hole in place

## Goal

A moq-transport fill (draft 22) or joining FETCH (draft 18) writes a group's
missing head into the same group the subscription's tail is writing, over a
disjoint object range, instead of the 7-state `Fill` claim, park, and install
handoff between two producers.

## Plan

From the 2026-10-24 pre-cut audit at 151a529670. Internal only.

Group stitching across two streams drew repeated fixes:
[#3308](https://github.com/moq-dev/moq/pull/3308),
[#3325](https://github.com/moq-dev/moq/pull/3325),
[#3562](https://github.com/moq-dev/moq/pull/3562),
[#3727](https://github.com/moq-dev/moq/pull/3727),
[#6251](https://github.com/moq-dev/moq/pull/6251),
[#6348](https://github.com/moq-dev/moq/pull/6348),
[#6407](https://github.com/moq-dev/moq/pull/6407), plus
[headless groups](/quest/m2/ietf-headless-group.md) and
[join after finish](/quest/m2/ietf-join-after-finish.md). The `Fill` enum
and its handoff (`rs/moq-net/src/ietf/subscriber.rs` about lines 273-396
and 4712-4862) exist only because the model has one producer per group.

Once [front eviction](/quest/m1/unbounded-groups/front-eviction.md) lets a
group be refilled in place, the head is a refill of the group's leading
hole, which also keeps a tail whose head never arrives, as the headless
group quest decided. Joining at the next group instead was considered and
rejected: it costs a GOP of startup and drops objects already received.

Public API: none. Wire: none.

## Required

- [Front eviction](/quest/m1/unbounded-groups/front-eviction.md) - in-place refill of a group's holes
- [moq-transport requests end through one lease](/quest/m2/ietf-request-lease.md) - restructures the same files first
