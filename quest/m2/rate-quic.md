# [L] QUIC enforces bitrate caps

## Goal

A relay session over QUIC (tokio and io_uring workers) never receives more
than its `publish.rate` or sends more than its `subscribe.rate`, whatever
the client does, and a capped session over QUIC is admitted instead of
refused.

## Plan

Token claims and HTTP grants carry path patterns only; object roles, including
bitrate caps, are refused during parsing. The first transport-enforcement quest
reintroduces optional caps with a type moq-auth owns (or a lower shared units
crate), converted at the relay boundary so moq-auth never depends on moq-net.
Its parsers and refusal rules must prevent any transport without enforcement
from silently discarding a cap. This quest reuses only caps that admission
actually enforces.

Decided in the 2026-10-04 plan: enforce in the QUIC stack, through
backpressure the peer cannot ignore, rather than metering and closing.

- **Ingress: pace flow-control credit.** The relay extends `MAX_DATA` at
  `rate`, with about one second of `rate` as the burst allowance. A peer
  that sends past its credit violates flow control and the connection fails,
  so a modified client cannot exceed the cap. An honest one goes
  flow-control limited (its congestion controller does not back off) and
  queues until [the grant](/quest/m2/rate-grant.md) clamps its encoder.
  Keep rate pacing separate from the connection memory allowance in
  message assembly (receive leases, [#5878](https://github.com/moq-dev/moq/pull/5878)). The memory allowance
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
  message. Memory-window reductions retain `moq-quic`'s shrink-as-debt
  semantics. Overshoot is
  bounded by credit outstanding at the change and the test asserts that bound.
- **Egress: cap the pacer.** The send rate is `min(controller rate, cap)`,
  so a subscriber below the broadcast's bitrate gets MoQ's normal group
  skipping, not a growing queue.
- **Runtime, per connection.** The cap is known only after auth (the token
  rides the CONNECT URL or arrives in band), so it is set on a live
  connection and reset on revalidation or a union change. Extend the
  runtime limits seam the relay widens cluster peers through
  (`set_limits` on `moq_tokio::server::Request::quic` and on
  `moq_uring::quic::web::Session`, both taking `moq_quic::Limits`) with the
  two rates, and reach it from the live session
  so revalidation can change it after SETUP.
- **Unions take the highest cap.** When in-band auth unions several grants
  on one session, each direction's cap is the highest among them, and an
  uncapped grant lifts it, matching the union's permissive meaning. Decided
  in the 2026-10-04 plan; `moq-auth` and `js/auth` carry no union helper
  until this quest needs one.
- Lands in `moq-quic`. QUIC admission accepts capped roles only once the stack enforces them.

Tests, on a simulated clock: a peer sending flat out is held to the cap
within the burst; a peer that ignores credit is closed with a flow-control
error; a datagram flood is held to the cap, excess dropped; a mixed stream
and datagram publisher under the cap loses no datagrams; a peer holding
unspent pre-auth credit, and one whose cap drops with credit outstanding,
stay within the stated bound; an uncapped session gets its normal window
after auth; egress to a capped subscriber never exceeds the cap; raising and
lowering the cap on a live connection takes effect; a union
of capped grants takes the highest cap and an uncapped grant lifts it; the io_uring path does
the same. Also prove that a frame larger than one rate interval completes
within the memory cap, an idle sender cannot accumulate a larger burst,
assembly cancellation does not refund rate charges, and intentional rate
pacing alone does not trigger pressure cancellation.

Decided in the 2026-10-10 receive-memory interview: memory allowance and
rate pacing are independent bounds. This replaces the earlier assumption
that a one-second rate window can also be the complete assembly allowance.
