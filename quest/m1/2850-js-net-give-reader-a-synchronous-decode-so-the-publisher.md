# [XS] js/net: cap the publisher's subscription controls

## Goal

A peer flooding `SUBSCRIBE_UPDATE` while the lite publisher is blocked in a
control-stream write cannot grow its memory without bound: past a fixed cap
the session fails with a protocol error.

## Plan

`SubscriptionControls` in `js/net/src/lite/publisher.ts` (:169) decodes
ahead of the serving loop so every buffered update applies before the next
group pop. It keeps only the newest range for the loop, but the decoder runs
on its own and calls `apply` for each update, so a peer can turn
flow-controlled bytes into unbounded work and heap while the loop is stalled.
First confirm what still grows on the current code. The 2026-10-05 audit
found the hole may already be closed: since #2820 `SubscriptionControls`
coalesces to the newest update ("Coalescing bounds memory") and each `apply`
is a single signal set, so per-update heap growth has no obvious source.
Flood SUBSCRIBE_UPDATE during a stalled write and measure; if nothing grows,
close #2850 and delete this quest instead. Otherwise cap the updates
decoded between two drains by the loop and fail the session on overflow,
matching Rust's refusal of malformed input. Test: a flood during a stalled
write fails the session instead of growing.

Decided in the 2026-09-30 audit: the rewrite to synchronous decoders is
dropped. Generated lite from the [rs2ts line](/quest/m1/rs2ts/README.md)
replaces hand-written js/net, and it gets control-first ordering from moq-net's
`poll_decode_maybe`. This quest only closes the memory hole until then.

## Closes

- [#2850](https://github.com/moq-dev/moq/issues/2850) - close this issue when the quest finishes
