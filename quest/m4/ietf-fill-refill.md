# [M] A moq-transport group head is a flight

## Goal

A moq-transport fill (draft 22) or joining FETCH (draft 18) serves a group's
missing head as a flight
([moq-dev/moq#7944](https://github.com/moq-dev/moq/pull/7944)): a separate
copy whose readers hand over to the subscription's tail copy at the first
object the tail holds, instead of the 7-state `Fill` claim, park, and install
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
[#7322](https://github.com/moq-dev/moq/pull/7322) (headless groups) and
join after finish ([moq-dev/moq#7306](https://github.com/moq-dev/moq/pull/7306)). The `Fill` enum
and its handoff exist only because the model has one producer per group. In
`rs/moq-net/src/ietf/subscriber.rs` on a86433043 (refreshed 2026-11-12):
`Fill`, `FillRequest`, and `Filling` at lines 308-498, `poll_claim_fill` at
6655, and `recv_fill` and `run_fill` from 6753. Search by those names if the
lines have moved again.

Decided 2026-10-28 (maintainer): nothing is written into a group copy below
its live log, so the head no longer fills the tail's group in place.
[Flights](https://github.com/moq-dev/moq/pull/7944) have landed (8cd44b57f),
so the head becomes a flight from object 0 that hands over to the tail copy, which also keeps a tail
whose head never arrives, as the headless group quest decided. Joining at the next group instead was considered and
rejected: it costs a GOP of startup and drops objects already received.

Public API: none. Wire: none.

## Required

- [moq-transport requests end through one lease](/quest/m4/ietf-request-lease.md) - restructures the same files first
