# [L] js/net: decode messages synchronously from buffered bytes

## Goal

The lite publisher applies every buffered control before it pops a group,
with no read-ahead queue: bounded memory and exact control-first ordering at
once. Fixes on dev, where the queue lives.

## Plan

The serving loop must apply every buffered `SUBSCRIBE_UPDATE` before it pops a
group, or a group goes out under a range the peer already superseded. Rust gets
that from `poll_decode_maybe` in `rs/moq-net/src/lite/publisher.rs`, which
decodes straight out of the reader's buffer and so drains controls to
exhaustion in one poll. On dev, `js/net/src/lite/publisher.ts` works around
the async `Reader` by decoding ahead into `SubscriptionControls`, which the
loop drains synchronously. That queue is unbounded: it grows while the loop is
blocked in a control-stream write, so a peer flooding updates during a stalled
write converts flow-controlled bytes into heap objects. A single-message slot
was tried in #2820 and broke ordering, because `take()` cannot yield to the
decoder without letting a group pop slip in between.

Every `Reader` primitive in `js/net/src/stream.ts` (`u62`, `u53`, `read`,
`string`, ...) is async and routes through a fill, even when the bytes are
already buffered. All 23 `static async decode` message decoders under
`js/net/src/lite/` are written against it, plus four `decodeMaybe` variants.

- Give `Reader` a synchronous decode over its buffer: the primitives read from
  `#buffer` and signal "incomplete" when it runs short, and one generic async
  driver fills and retries. The decoders then have a single synchronous body
  each; the async form is the driver applied to it, not a second copy.
- Convert all 23 decoders, so the `Reader` has one contract rather than a
  sync path for control messages and an async one for the rest.
- The publisher drains controls synchronously in its loop and
  `SubscriptionControls` goes away.
- Tests: a decoder given a partial buffer reports incomplete without
  consuming; the publisher applies N buffered updates before the next group
  pop; a partial update with a group already ready waits for the second fill
  and pops the group under the new range, so incomplete is never read as "no
  control pending"; the flood case stays bounded.

## Closes

- [#2850](https://github.com/moq-dev/moq/issues/2850) - close this issue when the quest finishes
