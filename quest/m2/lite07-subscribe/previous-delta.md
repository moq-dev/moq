# [L] Replace lite-07 SUBSCRIBE_SKIP and stall timers with Previous Delta

## Goal

On lite-07, every `GROUP` header carries `Previous Delta (i)` to prove gaps in the data layer, `SUBSCRIBE_SKIP` is removed completely, `GROUP_END` stays at stream end for whole-vs-closed and presentation `End`, and all five stall-guessing timers are removed from lite-07 (remaining isolated to pre-lite-07 and `moq-transport`). `/verify-subscribe` (`SubscribeAccounting.tla`) proves zero-timer settlement on the new wire.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan): `Previous Delta (i)` on the lite-07 `GROUP` header (`0` unknown, `1` every key below is empty, `d + 2` names `Previous Start = Group Start - (d + 1)`), `SUBSCRIBE_SKIP` removed from lite-07, `GROUP_END` kept.

[moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626) carries the wire, the Rust and JS ports, the draft, the `SubscribeAccounting.tla` proof (CutGroupEnd passes), and the Rust lite-07 copy timers (session tail, track tail, copy miss grace, resume copy retention). What remains waits on the decisions below:

- JS lite-07 graces: `Tail` (`js/net/src/tail.ts`), `Producer.#grace` (`js/net/src/track.ts`), and the subscription-end `settle(…, GRACE)` (`js/net/src/lite/subscriber.ts`) still wait a grace. The tail's excused-stream bound exists because a browser can lose a stream's reset.
- `S12` (`Producer::finish_shared` keeping `End::tail` on timed tracks and errored ends) is reached only from `moq-transport` (`ietf/`), never from a lite-07 copy, so it lies outside this epic's lite-07 scope.

## Waiting

- Maintainer: whether JS lite-07 drops its graces as Rust's did, including the excused-stream bound a browser that loses a reset relies on. Recommended: drop the tail and track graces, keep the excused-stream bound until WebTransport exposes `RESET_STREAM_AT`. Settled once the answer is in this quest's Plan.
- Maintainer: where `S12` goes. Recommended: move it to [Subscribe progress and cache-invariant fixes](/quest/m1/subscribe-progress-fixes.md), which owns non-wire fixes. Settled once that quest or this one lists it.
