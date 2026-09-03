# [M] js/net: give Reader a synchronous decode so the publisher need not read controls ahead

## Goal

Implement and verify the behavior tracked in [#2850](https://github.com/moq-dev/moq/issues/2850)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Split out of the review on https://github.com/moq-dev/moq/pull/2820.

The lite publisher's serving loop must apply every buffered control before it pops a group, or a group goes out under a subscription range the peer has already superseded. In Rust that falls out of `poll_decode_maybe` (`rs/moq-net/src/lite/publisher.rs`), which decodes a message straight out of the reader's buffer synchronously, so the loop can drain controls to exhaustion in one poll.

Nothing on the JS side can decode synchronously: `Reader`'s primitives (`u62`, `u53`, `read`) are all async, so `SubscribeUpdate.decodeMaybe` yields microtasks even when every byte is already buffered. The publisher works around this by decoding ahead into a queue (`SubscriptionControls` in `js/net/src/lite/publisher.ts`), which the loop drains synchronously.

That is correct but leaves the queue unbounded. It only grows while the serving loop is blocked in a control-stream write (SUBSCRIBE\_START / SUBSCRIBE\_END), and the loop drains it in one synchronous burst, so it stays small in practice. A peer that floods SUBSCRIBE\_UPDATE while such a write is stalled can still grow it without a ceiling, converting flow-controlled bytes into heap objects.

Bounding it naively does not work: a single-message slot was tried in #2820 and broke control-first ordering, because `take()` cannot yield to the decoder without letting a group pop slip in between. The fix is to remove the read-ahead instead of capping it.

Sketch: give `Reader` a way to attempt a decode using only buffered bytes, returning undefined when the message is incomplete rather than awaiting the transport (the `poll_decode_maybe` equivalent). The publisher then drains controls synchronously in its loop with no queue at all, bounded memory and exact ordering both. This needs a synchronous path through the message decoders, which today are written against the async reader, so it is not a small change.

Not urgent: the current behavior is correct, and the unbounded case needs a hostile peer plus a stalled control write.

## Closes

- [#2850](https://github.com/moq-dev/moq/issues/2850) - close this issue when the quest finishes
