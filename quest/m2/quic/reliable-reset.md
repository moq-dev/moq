# [L] Implement QUIC reliable stream reset

## Goal

noq-proto and its WebTransport adapter implement
[QUIC Stream Resets with Partial Delivery](https://datatracker.ietf.org/doc/draft-ietf-quic-reliable-stream-reset/).
A reset WebTransport data stream reliably delivers enough prefix bytes for the
receiver to associate it with its session before surfacing the application
error.

This supplies the missing transport feature tracked by
[Quinn#2676](https://github.com/quinn-rs/quinn/issues/2676). WebTransport over
HTTP/3 draft 16 requires `RESET_STREAM_AT` with a Reliable Size covering the
WebTransport stream header; ordinary `RESET_STREAM` is not compliant because
the receiver may discard that header and cannot attribute the reset.

## Plan

Implement the current reliable-reset draft in noq-proto:

- advertise and validate the empty `reset_stream_at` transport parameter,
  including remembered negotiation state for 0-RTT;
- encode, decode, acknowledge, lose, and retransmit `RESET_STREAM_AT` frames;
- retain and retransmit STREAM data below the smallest Reliable Size while
  abandoning data above it;
- enforce Reliable Size, Final Size, flow-control, repeated-reset, FIN, and
  STOP_SENDING state transitions and their specified connection errors;
- withhold the receive-side reset until the reliable prefix is deliverable,
  while preserving the application error and final size.

Expose a typed send-stream operation that takes an application error and
reliable prefix length. It must fail before mutating stream state when the
peer did not negotiate support or the requested sizes are invalid. Keep the
ordinary immediate reset operation for callers that need no reliable prefix.

The WebTransport layer owns its framing offset. When it resets a data stream,
it adds the encoded stream type or signal value and session ID to the
application's reliable prefix, and always commits at least through that
header. Callers must not calculate or pass the hidden WebTransport header
length. Preserve WebTransport application error codes unchanged on send,
receive, and intermediary forwarding.

Use the same state machine from qmux. Because qmux runs over a reliable ordered
transport, serialization acknowledges the committed prefix immediately, but
the receiver must still delay the reset until that prefix is available. Remove
the qmux prototype's local `RESET_STREAM_AT` state once the shared core owns it.

Test negotiation on/off and 0-RTT, loss and reordering of both the frame and
prefix data, shrinking Reliable Size, reset after FIN, flow-control blocking,
STOP_SENDING, malformed sizes, and resource cleanup. Add an end-to-end
WebTransport regression that opens a stream, writes no application payload,
resets immediately, and proves the peer receives the session association and
error instead of timing out. Exercise it through the Chrome wasm harness and
the native interop matrix.

Track the unversioned draft during implementation. The planning baseline is
draft 10, with transport parameter `0x1d` and frame type `0x24`; do not freeze
provisional codepoints if the document changes before release.

## Required

- [Establish the noq relationship](/quest/m2/quic/parent.md) - the frame and
  stream-state implementation is proposed to noq first

## Related

- [qmux on the QUIC stream state machine](/quest/m2/quic/qmux.md) - consumes
  the same reset state without a parallel implementation
- [noq parity gate](/quest/m2/quic/noq-parity.md) - cannot claim
  WebTransport parity without reliable reset
