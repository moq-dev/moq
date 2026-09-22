# [S] A control timeout is not a delivery timeout

## Goal

A request stream torn down because the peer never answered resets with
CONTROL_TIMEOUT rather than DELIVERY_TIMEOUT. Both languages agree on the
moq-lite code, the IETF wire says INTERNAL_ERROR, and a relay carrying the
reset across a hop does not change what it says.

## Plan

`DELIVERY_TIMEOUT` describes content that missed its deadline (draft-20 section
3.3.4, and the same claim moq-lite makes with 0x2). Every local timeout uses it
today, including the ones that never touched content:

- `js/net/src/ietf/subscriber.ts:434` aborts the request stream with the
  `SUBSCRIBE_OK` timer's `TimeoutError`, and `js/net/src/ietf/publisher.ts`
  does the same with the PUBLISH_NAMESPACE response timer (both through
  `withTimeout`). `localStreamCode` in `js/net/src/error.ts:288` maps every
  `TimeoutError` to `StreamCode.DeliveryTimeout`.
- Rust does the same through `From<&Error> for StreamError`
  (`rs/moq-net/src/error.rs:587`): `Error::Timeout` becomes
  `StreamError::DeliveryTimeout`.

So a peer that opens a stream and goes quiet is told its content was late.

Decided:

- **The code is 0x31 CONTROL_TIMEOUT**, in moq-lite's own 48-63 range
  (`drafts/draft-lcurley-moq-lite.md:268`; 0x30 NO_CAPACITY at `:311`, 0x32
  is GROUP_TOO_LARGE). It mirrors the
  session code 0x11 CONTROL_MESSAGE_TIMEOUT (`:287`, `SessionError::Timeout`
  at `rs/moq-net/src/error.rs:79`): one condition, two scopes. Add the row to
  the stream table and `StreamError::ControlTimeout` beside `DeliveryTimeout`.
- **`Error::Timeout` is not split.** Only the wire code differs: the control
  paths that time out a response abort their stream with
  `StreamError::ControlTimeout` directly instead of through the `From<&Error>`
  mapping, and `ControlTimeout` decodes back to `Error::Timeout` the way
  `DeliveryTimeout` does (`error.rs:511`). js/net mirrors it with
  `StreamCode.ControlTimeout: 0x31` (`js/net/src/error.ts:71-95`), and the two
  control timers pass it explicitly rather than relying on the `TimeoutError`
  mapping, which keeps meaning delivery.
- **IETF stays INTERNAL_ERROR.** The moq-transport registry has no value for
  it: `to_stream_code` (`rs/moq-net/src/ietf/error.rs:78`) falls through at
  `:92`, and `sharedStreamCode` (`js/net/src/error.ts:281`) gates the same
  way. Nothing to add there.
- **Ships on main.** `StreamError` is `#[non_exhaustive]` (`error.rs:125`),
  so the variant is additive, and a wire change alone does not need dev.

Tests: `stream_codes_round_trip` (`error.rs:691`) gains `ControlTimeout` in
its registered list; the js `error.test.ts` mirror does the same; a subscribe
whose `SUBSCRIBE_OK` never arrives resets with 0x31 on lite and INTERNAL_ERROR
on every IETF draft.

## Related

- GROUP_TOO_LARGE (0x32) - neighbouring code in the same range
