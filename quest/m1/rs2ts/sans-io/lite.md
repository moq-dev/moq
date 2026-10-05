# [L] Sans-IO lite session

## Goal

The lite session is a state machine fed bytes, stream open and close events,
and `tick(now)`; it returns bytes to write and events for the model. No
`web_transport_trait` stream types, no timers of its own, no async outside
the `async` feature.

## Plan

A hand-carved spike (lite-06 subscriber: SETUP, ANNOUNCE, SUBSCRIBE, group
streams, deadlines via `tick`) came to about 600 lines with frames surfaced
as `[offset, len]` ranges into the caller's chunk, so payload bytes are never
copied by the core. Decode every complete frame already buffered in one pass;
awaiting per frame is where js/net loses 2.5-3.5 µs per frame.

Guidance:

- Keep the session's existing behavior and wire exactly; the interop suite is
  the check.
- Stream handles, write backpressure, and close codes become explicit events
  or return values the driver acts on.
- Keep maps as Vec slabs where the key space is small.
- Turn the session's async test bodies into synchronous `poll_*` tests with
  explicit instants as it is rewritten, so they translate with the code.

Public API: breaks moq-net's session API. Wire: none.
