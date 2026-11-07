# [M] kio's Instant has no panicking arithmetic, and peer values never wrap

## Goal

No value a peer sends or an application configures can panic a moq task or silently wrap:

- `kio::clock::Instant` drops `Add`, `AddAssign`, `Sub` and `SubAssign` with `Duration` (`rs/kio/src/clock/instant.rs`), which panic past the clock's range. Every call site picks `checked_add`/`checked_sub` with an explicit meaning for overflow (no deadline, refused) or a saturating form. This removes the class behind, among others, the IETF publisher's REQUEST_ERROR retry interval (`rs/moq-net/src/ietf/publisher.rs` `refusal`, which any peer can panic) and `rs/moq-net/src/reconnect.rs` deadlines from huge configured durations.
- Peer-supplied counters never wrap with `+ 1`: the ietf `save_join` `largest.object + 1`, the lite SUBSCRIBE_DROP end `u64::MAX` (`saturating_add(1)` that misses that group in `lite/subscriber.rs` and `subscriber/fills.rs`), and the router's advertised seqno in `route/mod.rs`.
- A datagram sequence of 2^62 or more relayed to a lite-05/06 subscriber is handled explicitly in `lite/publisher.rs` `DatagramSend::send` instead of dropped silently.

## Plan

Found by the 2026-11-07 invariant audit (ietf-1, ietf-2, lite-3, lite-4, lite-9, model-3).

Decided 2026-11-07 (maintainer, invariant-audit root causes): remove the operators rather than make them saturate, so each of the roughly 600 sites states what overflow means; most edits are mechanical. Breaking for the published `kio` crate; it stays in m0 because a remote peer can panic the publisher today. Removing `Sub` alongside `Add` is the same class (underflow before the clock's start). Each peer-reachable site gets a test at the extreme value.
