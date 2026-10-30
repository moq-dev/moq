# [M] A moq-transport group head is a flight

## Goal

A moq-transport fill (draft 22) or joining FETCH (draft 18) serves a group's
missing head as a [flight](/quest/m1/unbounded-groups/flights.md): a separate
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
[join after finish](/quest/m2/ietf-join-after-finish.md). The `Fill` enum
and its handoff (`rs/moq-net/src/ietf/subscriber.rs` about lines 273-396
and 4712-4862) exist only because the model has one producer per group.

Decided 2026-10-28 (maintainer): nothing is written into a group copy below
its live log, so the head no longer fills the tail's group in place. Once
[Flights](/quest/m1/unbounded-groups/flights.md) lands, the head is a flight
from object 0 that hands over to the tail copy, which also keeps a tail
whose head never arrives, as the headless group quest decided. Joining at the next group instead was considered and
rejected: it costs a GOP of startup and drops objects already received.

Public API: none. Wire: none.

## Required

- [Flights](/quest/m1/unbounded-groups/flights.md) - separate paced copies and the handover between them
- [moq-transport requests end through one lease](/quest/m2/ietf-request-lease.md) - restructures the same files first
