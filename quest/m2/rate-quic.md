# [L] QUIC enforces bitrate caps

## Goal

A relay session over QUIC (tokio and io_uring workers) never receives more
than its `publish.rate` or sends more than its `subscribe.rate`, whatever
the client does, and a capped session over QUIC is admitted instead of
refused.

## Plan

Decided in the 2026-10-04 plan: enforce in the QUIC stack, through
backpressure the peer cannot ignore, rather than metering and closing.

- **Ingress: pace flow-control credit.** The relay extends `MAX_DATA` at
  `rate`, with a window of about one second of `rate` as the burst. A peer
  that sends past its credit violates flow control and the connection fails,
  so a modified client cannot exceed the cap. An honest one goes
  flow-control limited (its congestion controller does not back off) and
  queues until [the grant](/quest/m2/rate-grant.md) clamps its encoder.
  Per-stream windows are unchanged; the connection credit is the cap.
- **Datagrams draw from the same allowance.** DATAGRAM frames are not flow
  controlled, so credit alone does not bound them. Received datagram payload
  is charged to the same bucket that paces `MAX_DATA` credit, and a datagram
  arriving with the bucket empty is dropped before routing, the same as
  network loss. An honest publisher is never disconnected, so this does not
  wait for [the grant](/quest/m2/rate-grant.md).
- **Credit cannot be retracted** (RFC 9000 §4.1). Every session, capped or
  not, starts with an `initial_max_data` shrunk to what the handshake,
  CONNECT, SETUP, and in-band AUTH need, with no config knob, so a peer
  cannot bank a full default window and spend it after a low cap arrives.
  After auth the relay raises it through the `set_limits` seam: the normal
  window for an uncapped session, paced credit for a capped one. A lowered
  cap (revalidation, a union shrinking) uses the shrink-as-debt behavior
  [peer limits](/quest/m1/quic/peer-limits.md) relies on, so the overshoot is
  bounded by the credit outstanding at the change and the test asserts that
  bound.
- **Egress: cap the pacer.** The send rate is `min(controller rate, cap)`,
  so a subscriber below the broadcast's bitrate gets MoQ's normal group
  skipping, not a growing queue.
- **Runtime, per connection.** The cap is known only after auth (the token
  rides the CONNECT URL or arrives in band), so it is set on a live
  connection and reset on revalidation or a union change. Reuse the runtime
  limits seam [peer limits](/quest/m1/quic/peer-limits.md) adds
  (`set_limits(Limits)` on the `web-transport-moq` session), extended with
  the two rates.
- Lands in `moq-quic`, so it waits for the [fork](/quest/m1/quic/fork/README.md).
  The relay drops its refusal for QUIC sessions in the same PR.

Tests, on a simulated clock: a peer sending flat out is held to the cap
within the burst; a peer that ignores credit is closed with a flow-control
error; a datagram flood is held to the cap, excess dropped; a peer holding
unspent pre-auth credit, and one whose cap drops with credit outstanding,
stay within the stated bound; an uncapped session gets its normal window
after auth; egress to a capped subscriber never exceeds the cap; raising and
lowering the cap on a live connection takes effect; the io_uring path does
the same.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the credit and pacer changes land in `moq-quic`
- [Bitrate claim](/quest/m2/rate-claim.md) - the cap this enforces

## Related

- [Peer limits](/quest/m1/quic/peer-limits.md) - the same runtime limits seam, raised for cluster peers
