# [M] JS session stats count send-credit stalls

## Goal

`@moq/net` over the WebSocket fallback (`@moq/qmux`) reports time spent blocked
on the peer's `MAX_DATA` and the number of blocked episodes, matching the Rust
session stats' `credit_stalls` and `credit_stalled`, under the same names.

Non-goals: browser WebTransport, which exposes no credit state.

## Plan

Split from the Rust quest on 2026-11-21 (maintainer, /quest-plan of the main →
release flow-control review): ✅ its own quest (rejected: keeping it in the
Rust quest, which the cut waits on). It starts from zero: `js/qmux` ignores
the blocked frames and `js/net`'s WebSocket stats report nothing. Count on the
sending side with the Rust episode rule (`rs/moq-quic`'s `SendCreditStats`), so
both languages count the same thing: an episode starts when a write blocks with
every byte the peer's `MAX_DATA` allows already sent, and ends once the peer
raises it, no writer still waits, or the connection closes. A write held only by
the local send buffer is not one, and received DATA_BLOCKED frames are not
counted.
