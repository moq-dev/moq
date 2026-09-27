# [M] js/net: decode messages synchronously from buffered bytes

## Goal

The lite publisher applies every buffered control before it pops a group,
with no read-ahead queue: bounded memory and exact control-first ordering at
once.

## Plan

The serving loop must apply every buffered `SUBSCRIBE_UPDATE` before it pops a
group, or a group goes out under a range the peer already superseded. Rust gets
that from `poll_decode_maybe` in `rs/moq-net/src/lite/publisher.rs`, which
decodes straight out of the reader's buffer and so drains controls to
exhaustion in one poll. `js/net/src/lite/publisher.ts` works around
the async `Reader` by decoding ahead into `SubscriptionControls`, which the
loop drains synchronously. That queue is unbounded: it grows while the loop is
blocked in a control-stream write, so a peer flooding updates during a stalled
write converts flow-controlled bytes into heap objects. A single-message slot
was tried in #2820 and broke ordering, because `take()` cannot yield to the
decoder without letting a group pop slip in between.

`Reader` in `js/net/src/stream.ts` already decodes synchronously: a decode is
a function over a `Cursor`, whose reads throw an internal short signal when the
buffered bytes run out. `tryDecode` returns undefined and consumes nothing in
that case, and `decode`/`decodeMaybe` are the one async driver that fills and
retries. The primitives (`u62`, `u53`, `read`, `string`, ...) are that driver
applied to the `Cursor` reads, and the group and FETCH frame loops drain every
buffered frame with `tryDecode` (`js/net/bench/frames.ts`). The 23
`static async decode` message decoders under `js/net/src/lite/`, plus four
`decodeMaybe` variants, still await a primitive per field.

- Convert all 23 decoders to a single synchronous body over a `Cursor`, with
  the async form as `reader.decode(...)` rather than a second copy. `Message`
  in `lite/message.ts` becomes a sync size-prefixed wrapper.
- The publisher drains controls synchronously in its loop and
  `SubscriptionControls` goes away.
- Tests: the publisher applies N buffered updates before the next group
  pop; a partial update with a group already ready waits for the second fill
  and pops the group under the new range, so incomplete is never read as "no
  control pending"; the flood case stays bounded.

## Closes

- [#2850](https://github.com/moq-dev/moq/issues/2850) - close this issue when the quest finishes
