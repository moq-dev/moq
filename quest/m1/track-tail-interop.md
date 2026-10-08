# [S] Track tail interop

## Goal

`just test interop` covers a publisher that ends a track while its last group
is still in flight: a Rust publisher's track is read to its declared end by the
JS subscriber, and a JS publisher's by the Rust subscriber, through the relay.
Each reader gets every group below the end and then a clean end, never an
error or a stall.

## Plan

Both moq-net and `@moq/net` wait for a track's tail once the publisher ends a
subscription, and each is covered by its own in-process tests. Nothing checks
that the two agree across a relay on real QUIC.

What stood in the way when the Rust half landed:

- `moq import` exiting the moment stdin ends is cleared: since #4430 a
  graceful close delivers finished tracks at stdin EOF.
- The native JS subscriber (`test/interop/clients/js-native`) returns on the
  first frame. It needs a mode that reads a track to its end and reports how it
  ended and which groups it saw.

The harness exposed a relay start-floor defect: when a newer group arrives
first, earlier in-flight groups can be lost. #4387 (merged 09-28) fixed that
case, but the tail lanes still fail on this quest's PR #4225 after it: Interop
run 37139033242 (2026-10-03) failed `tail rust -> rust`, `rust ->
js-native-node`, and `rust -> js-native-bun`. Merge `main` (after #4741 and
#4813) into #4225, rerun, and record here whatever defect still fails.

Found 2026-10-08: #4225's `go -> *` lanes fail on the
[serve budget](/quest/m0/serve-budget.md) stall, where the go publisher's
serve loop starves its own QUIC driver until the relay times it out. Rerun
the lanes once that lands.

Decided in the 2026-09-30 audit: the lite-07 drop case moved into
[SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md)'s tests, so the basic Rust and
JS tail interop lands now instead of waiting on that [L] quest.

QUIC on localhost rarely reorders, so this is a smoke check that the end is
delivered and clean. The ordering race itself stays in the unit tests.

## Required

- [Serve budget](/quest/m0/serve-budget.md) - fixes the stall that fails #4225's go lanes

## Related

- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - owns the lite-07 drop case on top of this harness
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - keeps a reset stream's header, so the reset acts as a one-group drop
