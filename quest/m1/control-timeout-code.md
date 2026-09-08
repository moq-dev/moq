# [S] A control timeout is not a delivery timeout

## Goal

A request stream torn down because the peer never answered resets with a code
that says so, rather than DELIVERY_TIMEOUT. Both wires and both languages agree,
so a relay carrying the reset across a hop does not change what it says.

## Plan

`DELIVERY_TIMEOUT` describes content that missed its deadline (draft-20 section
3.3.4, and the same claim moq-lite makes with 0x2). Every local timeout uses it
today, including the ones that never touched content:

- `js/net/src/ietf/subscriber.ts` aborts the request stream with the
  `SUBSCRIBE_OK` timer's `TimeoutError`, and `js/net/src/ietf/publisher.ts` does
  the same with the PUBLISH_NAMESPACE response timer. `toStreamCode` in
  `js/net/src/error.ts` maps every `TimeoutError` to `StreamCode.DeliveryTimeout`.
- Rust does the same through `From<&Error> for StreamError` in
  `rs/moq-net/src/error.rs`: `Error::Timeout` becomes
  `StreamError::DeliveryTimeout`.

So a peer that opens a stream and goes quiet is told its content was late.

The fix is a condition of its own for "you did not answer", not a rename of the
existing one, since a real delivery deadline still needs DELIVERY_TIMEOUT. The
moq-transport stream registry has no value for it, so the IETF wire says
INTERNAL_ERROR; moq-lite can register one in its reserved range or say the same.
Whichever it says, `rs/moq-net/src/error.rs`, `js/net/src/error.ts` and
`rs/moq-net/src/ietf/error.rs` have to agree, and the moq-lite draft changes with
them if a value is registered.

Decide first whether the distinction is worth a code at all: a caller that only
logs the reason gains nothing, and INTERNAL_ERROR is already what a peer does
with an unregistered value. If it is not, the fix is instead to stop putting a
control timeout through the delivery mapping and let it be INTERNAL_ERROR
outright, which is a smaller change than a new code.

## Related

- [Group overflow](/quest/m1/group-overflow-abort.md) - the other condition whose wire code is under review
