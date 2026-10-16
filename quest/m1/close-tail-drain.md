# [M] Subscribers drain the tail after a peer close

## Goal

A group the publisher finished before a graceful session close reaches the
subscriber on every version (lite-01 through lite-07, and IETF): after a peer
close, the subscriber keeps reading the group streams its transport already
delivered instead of aborting them. Reproduce the loss first with a regression
test that fails without the fix. No wire change.

## Plan

Found while reworking the in-memory transport (#5530). Facts at 813a92b48:

- On a peer close, lite `UniAccept::poll` returns the accept error
  (`rs/moq-net/src/lite/subscriber.rs:712`), and `Driver::poll`
  (`rs/moq-net/src/lite/session.rs:381-411`) aborts every subscription when
  `local_close` is false, then drops the accepted `UniServe` children
  mid-read. IETF `run_unis` (`rs/moq-net/src/ietf/session.rs:882`,
  `:931-942`) does the same, and `ietf::session::start` aborts again after
  the race in each of its two arms, drafts 14-16 (`:371-374`) and drafts 17+
  (`:588-591`).
- Real QUIC keeps that data readable: moq-tokio's `quic::driver` `poll_accept` hands out
  already-received streams before the connection error, and reads return
  buffered chunks and the FIN, reporting `ConnectionLost` only when blocked.
  moq-uring's raw QUIC follows the same order, but its WebTransport layer
  does not: `poll_accept_uni` (`rs/moq-uring/src/quic/web.rs:566-569`)
  returns the connection error before classifying streams already in
  `pending_uni`, so a tail stream accepted in the same poll as the close is
  lost below the subscriber.
- The closer (`Session::close`, `rs/moq-net/src/session.rs:201`) waits for
  its group streams' FIN acks. An ACK only means delivery to the peer's
  transport, not that the peer processed it, so on lite-01 through lite-06
  and IETF the receiver throws away a tail its transport already holds.
  lite-07 already avoids this at the protocol level: the closer waits for the
  subscriber's Subscribe-stream FIN (`drafts/draft-lcurley-moq-lite.md`).
- The test mock (`rs/moq-net/tests/support/mock.rs`) hid it: its FIN counts as
  acked only once the reading application stops or drops the stream.
  `older_lite_close_keeps_ack_drain` never checks what the reader got.

Decided (maintainer, 2026-10-14):

- Fix at the receiver. After a peer close, the lite and IETF subscribers
  stop aborting accepted group streams and read each to its FIN or until the
  transport reports it closed. Buffered data bounds the drain, so no timer.
  Streams not yet accepted are still accepted and drained in the same way.
  The closer-side wait stays lite-07 only, since older published versions
  can't gain a wire change.
  Make the drain a session-level phase, not just a change to `UniAccept` and
  `run_unis`: a control-stream or publisher error can end the driver first,
  and subscription cleanup can remove a track a data stream still needs.
  Keep the subscription state the draining streams depend on, and settle
  anything that can't complete after the close (such as IETF children waiting
  on control state) so the no-timer argument holds. That argument needs a
  closed transport: a non-transport terminal error (such as legacy lite's
  session-stream FIN returning `Cancel`, `rs/moq-net/src/lite/session.rs:652-660`)
  must not start a drain over a live connection that could hang. Close the
  transport first, or abort as today if buffered data does not survive a
  local close.
- Tests ack a FIN on delivery, as QUIC does, so the regression for lite-01
  through lite-06 and IETF (one draft from each `start` arm) asserts the
  reader got the tail and fails before the fix. Cover both the buffered-tail
  case and one where a control or accept error wins the poll order. Give
  `older_lite_close_keeps_ack_drain` that reader assertion.
- Once #5530 has landed, switch `moq_net::transport::memory`'s
  `SendStream::poll_closed` to resolve on `finish` (delivery into the
  channel), dropping its "finished and empty" rule. Do this unless the
  memory bench (`rs/moq-net/benches/memory.rs`) shows a regression; if it
  does, change only the tests.

The receiver drain also needs moq-uring WebTransport to classify and hand
out its pending uni streams before reporting the connection error, with a
regression for a tail stream and CONNECTION_CLOSE arriving in the same poll.
Check the other `poll_accept_uni` adapters (such as `rs/moq-ffi` and
moq-tokio's `quic::web` path) for the same ordering.

Public API: none. Wire: none.

## Related

- [Publisher linger](/quest/m1/publisher-linger.md) - keeps a finished broadcast servable so subscribers can drain it, a layer above this
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - groups cut short by a session close
- [QUIC ack hook](/quest/m2/quic-ack-hook.md) - a FIN/ack hook in the QUIC layer
