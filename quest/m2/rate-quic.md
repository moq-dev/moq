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
  so a modified client cannot exceed the cap, and an honest one sees its
  congestion controller back off. Per-stream windows are unchanged; the
  connection credit is the cap.
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
error; egress to a capped subscriber never exceeds the cap; raising and
lowering the cap on a live connection takes effect; the io_uring path does
the same.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the credit and pacer changes land in `moq-quic`
- [Bitrate claim](/quest/m2/rate-claim.md) - the cap this enforces

## Related

- [Peer limits](/quest/m1/quic/peer-limits.md) - the same runtime limits seam, raised for cluster peers
