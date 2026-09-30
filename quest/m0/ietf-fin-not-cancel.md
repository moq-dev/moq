# [M] A request stream FIN is not a cancellation

## Goal

On moq-transport's bidi request streams (draft 17+), a peer's FIN means no
more updates, not cancel. Only RESET_STREAM, STOP_SENDING or our own end
cancels. Today a FIN withdraws a PUBLISH_NAMESPACE
(`ietf/subscriber.rs`), ends a SUBSCRIBE_NAMESPACE, and cancels a SUBSCRIBE
(`ietf/publisher.rs`). Draft-21 section 6.4.2.2 says a requester MAY FIN
right after its message. A REQUEST_UPDATE on a subscribe stream is never
parsed either: its first byte makes `poll_closed` return an error and
silently ends the subscription.

## Plan

- Separate the read side's clean FIN (`Reader::poll_closed` returning Ok)
  from a reset. On FIN, keep serving and stop reading. Watch the writer for
  STOP_SENDING.
- Parse REQUEST_UPDATE on subscribe streams, where the draft allows it.
  Apply what we support and refuse the rest `NOT_SUPPORTED`.
- Check the same FIN handling in `js/net`.
- Tests for each request type: FIN right after the request keeps it alive,
  a reset cancels it, and a REQUEST_UPDATE is applied.

Public API: none. Wire: conformance fix; no draft change.

## Related

- [Legal IETF input](/quest/m0/ietf-legal-input.md) - the other interop blocker
