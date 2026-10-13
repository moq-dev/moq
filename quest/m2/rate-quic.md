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
  `rate`, with about one second of `rate` as the burst allowance. A peer
  that sends past its credit violates flow control and the connection fails,
  so a modified client cannot exceed the cap. An honest one goes
  flow-control limited (its congestion controller does not back off) and
  queues until [the grant](/quest/m2/rate-grant.md) clamps its encoder.
  Keep rate pacing separate from the connection memory allowance in
  [message assembly](/quest/m1/quic/receive-memory.md). The memory allowance
  must fit the largest supported encoded unit even when it takes many rate
  intervals to receive. Limit each grant by both rate allowance and available
  memory backing; do not wait for a whole-message credit release to grant
  previously unadvertised headroom. Track retained received bytes separately
  from outstanding granted credit, bounding their combined memory commitment.
  Bound unspent grants as well, so an idle peer cannot bank a memory-sized
  burst. A large frame must finish gradually without enlarging the rate burst.
  Stream-credit replenishment remains independent.
- **Datagrams draw from the same allowance.** DATAGRAM frames are not flow
  controlled, so credit alone does not bound them. Received datagram payload
  is charged to the same allowance that paces `MAX_DATA` credit, and a
  datagram past the cap plus burst is dropped before routing, the same as
  network loss. Charge newly received stream bytes and datagram payload
  once, not application message completion or playback, so datagram bytes
  withhold the next credit extension: unspent stream credit does not starve
  datagrams, a publisher mixing both under the cap loses none, and mixed
  traffic overshoots by at most the outstanding burst credit. Cancellation
  frees memory but does not refund bytes against the rate allowance. An
  honest publisher is never disconnected for exceeding its rate, so this
  does not wait for
  [the grant](/quest/m2/rate-grant.md).
- **Credit cannot be retracted** (RFC 9000 §4.1). Every session, capped or
  not, starts with an `initial_max_data` shrunk to what the handshake,
  CONNECT, SETUP, and in-band AUTH need, with no config knob, so a peer
  cannot bank a full default window and spend it after a low cap arrives.
  After auth the relay raises it through the `set_limits` seam: the normal
  window for an uncapped session, paced credit for a capped one. A lowered
  rate (revalidation, a union shrinking) withholds future grants against
  outstanding credit rather than shrinking assembly memory below a legal
  message. Memory-window reductions retain the shrink-as-debt semantics
  described in [peer limits](/quest/m1/quic/peer-limits.md). Overshoot is
  bounded by credit outstanding at the change and the test asserts that bound.
- **Egress: cap the pacer.** The send rate is `min(controller rate, cap)`,
  so a subscriber below the broadcast's bitrate gets MoQ's normal group
  skipping, not a growing queue.
- **Runtime, per connection.** The cap is known only after auth (the token
  rides the CONNECT URL or arrives in band), so it is set on a live
  connection and reset on revalidation or a union change. Reuse the runtime
  limits seam [peer limits](/quest/m1/quic/peer-limits.md) adds
  (`set_limits(Limits)` on moq-net's transport trait, implemented by the
  moq-tokio and moq-uring adapters after the fork switch), extended with
  the two rates.
- Lands in `moq-quic`, so it waits for the [fork](/quest/m1/quic/fork/README.md).
  The relay drops its refusal for QUIC sessions in the same PR.

Tests, on a simulated clock: a peer sending flat out is held to the cap
within the burst; a peer that ignores credit is closed with a flow-control
error; a datagram flood is held to the cap, excess dropped; a mixed stream
and datagram publisher under the cap loses no datagrams; a peer holding
unspent pre-auth credit, and one whose cap drops with credit outstanding,
stay within the stated bound; an uncapped session gets its normal window
after auth; egress to a capped subscriber never exceeds the cap; raising and
lowering the cap on a live connection takes effect; the io_uring path does
the same. Also prove that a frame larger than one rate interval completes
within the memory cap, an idle sender cannot accumulate a larger burst,
assembly cancellation does not refund rate charges, and intentional rate
pacing alone does not trigger pressure cancellation.

Decided in the 2026-10-10 receive-memory interview: memory allowance and
rate pacing are independent bounds. This replaces the earlier assumption
that a one-second rate window can also be the complete assembly allowance.

## Required

- [Hard fork](/quest/m1/quic/fork/README.md) - the credit and pacer changes land in `moq-quic`
- [Peer limits](/quest/m1/quic/peer-limits.md) - adds the runtime `set_limits(Limits)` seam and shrink-as-debt behavior this extends
- [Bound message assembly](/quest/m1/quic/receive-memory.md) - separate retained memory from paced outstanding credit
- [Bitrate claim](/quest/m2/rate-claim.md) - the cap this enforces
