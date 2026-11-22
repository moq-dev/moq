# [M] JS session stats count send-credit stalls

## Goal

`@moq/net` over the WebSocket fallback (`@moq/qmux`) reports time spent blocked
on the peer's `MAX_DATA` and the number of blocked episodes, matching the Rust
counters [send-credit stalls](/quest/m1/send-credit-stalls.md) adds, under the
same names.

Non-goals: browser WebTransport, which exposes no credit state.

## Plan

Split from the Rust quest on 2026-11-21 (maintainer, /quest-plan of the main →
release flow-control review): ✅ its own quest (rejected: keeping it in the
Rust quest, which the cut waits on). It starts from zero: `js/qmux` ignores
the blocked frames and `js/net`'s WebSocket stats report nothing. Count on the
sending side with the Rust quest's episode rule, once that lands, so both
languages count the same thing.

## Required

- [Send-credit stalls](/quest/m1/send-credit-stalls.md) - the episode rule and field names this mirrors
