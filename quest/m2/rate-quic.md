# [L] QUIC enforces bitrate caps

## Goal

A relay session over QUIC (tokio and io_uring workers) never receives more
than its `publish_rate` or sends more than its `subscribe_rate`, whatever
the client does, and a capped session over QUIC is admitted instead of
refused.

## Plan

The stack half landed in [#7094](https://github.com/moq-dev/moq/pull/7094):
`moq_quic::Limits` carries `receive_rate` and `send_rate` (a
`moq_quic::Rate`, bytes per second or unlimited), re-exported by both
runtimes and set through `set_limits` on a live connection or through
`transport::Config::limits` at construction (QMux ignores them).

- A receive cap paces `MAX_DATA` from a token bucket holding one second of
  `rate`, granted in eighths of it. Stream bytes and datagram payload are
  charged once on arrival; a datagram the bucket cannot hold is dropped, and
  stream bytes spending credit a datagram already used overdraw the bucket,
  withholding the next grant. A peer sending past its credit is a
  `FLOW_CONTROL_ERROR`.
- The memory window (receive leases) is a separate bound: credit is limited
  by both, eviction looks only at the window, so pacing alone never evicts,
  and a frame larger than a burst completes gradually.
- A new cap starts with a full burst that counts the credit outstanding; a
  lowered cap keeps outstanding credit as the only overshoot; lifting it
  grants the window at once.
- A send cap gates ack-eliciting packets beside the controller's pacer,
  retransmissions included, and reports the hold as application-limited.

Decided 2026-10-31 (maintainer) on [#7094](https://github.com/moq-dev/moq/pull/7094): the stack half lands alone; the
send cap counts ack-eliciting packets, QUIC overhead and retransmissions
included, since that bounds what a subscriber can make the relay send,
faked loss included; and the egress burst is one second of the rate, as on
ingress, since the controller's pacer already spreads it at path rate.

What remains is the auth and relay half.

Token claims and HTTP grants carry path patterns only; object roles, including
bitrate caps, are refused during parsing. Decided 2026-10-25 (maintainer):
this quest reintroduces optional caps with a type moq-auth owns (or a lower
shared units crate), converted at the relay boundary so moq-auth never depends
on moq-net. [WebSocket caps](/quest/m2/rate-websocket.md) reuses that type,
and [the grant](/quest/m2/rate-grant.md) Requires this quest for it. Its
parsers and refusal rules must prevent any transport without enforcement from
silently discarding a cap: admission accepts a cap only on a transport that
enforces it. The audit that removed `Access` (2026-10-22) objected to
`grant.publish` becoming `grant.publish.path` and expected re-adding a rate to
be additive; released verifiers already refuse the `{"path","rate"}` object
role (`rs/moq-auth/tests/released_token.rs`), so a cap cannot be silently
discarded by an old relay.

- **The cap shape.** Decided 2026-10-31 (maintainer) on [#7094](https://github.com/moq-dev/moq/pull/7094): additive
  Rust fields, `publish_rate` and `subscribe_rate` beside `publish:
  Patterns` and `subscribe: Patterns`, so `grant.publish.matches(..)` stays,
  with the `{"path": [...], "rate": bps}` object role on the wire. `js/auth`
  mirrors it. Restoring `Access { path, rate }` was rejected for the
  `.path` ergonomics the 2026-10-22 audit removed it over.

- **Credit cannot be retracted** (RFC 9000 §4.1). Every session, capped or
  not, starts with an `initial_max_data` shrunk to what the handshake,
  CONNECT, SETUP, and in-band AUTH need, with no config knob, so a peer
  cannot bank a full default window and spend it after a low cap arrives.
  After auth the relay raises it through `set_limits`: the normal window for
  an uncapped session, paced credit for a capped one (set the rate with or
  before the window, as `set_limits` does). Today `[quic] receive_window`
  becomes `initial_max_data` directly in `moq-tokio` and `moq-uring`, and
  `moq-tokio` refuses a window below the largest message, so the server
  paths need a pre-auth window the relay then opens.
- **Runtime, per connection.** The cap is known only after auth (the token
  rides the CONNECT URL or arrives in band), so it is set on a live
  connection before SETUP OK and reset on revalidation or a union change.
  The tokio connection handle is `Send`; the io_uring one is not, so a
  re-check on the tokio side needs a path to the ring worker.
- **Unions take the highest cap.** When in-band auth unions several grants
  on one session, each direction's cap is the highest among them, and an
  uncapped grant lifts it, matching the union's permissive meaning. Decided
  in the 2026-10-04 plan; `moq-auth` and `js/auth` carry no union helper
  until this quest needs one.
- The relay's receive cap is the session's `publish_rate`, its send cap the
  `subscribe_rate`, converted from bits to bytes per second.
- QUIC admission accepts capped roles only on the moq-quic stack (tokio and
  io_uring); WebSocket, iroh, and HTTP `/fetch` and `/announced` keep refusing
  them, including a capped grant arriving in band or at revalidation.

Tests: a union of capped grants takes the highest cap and an uncapped grant
lifts it; a cap change at revalidation reaches the live connection; a capped
grant on a transport without enforcement is refused; a capped relay session
is held to its caps on both runtimes; an uncapped session gets its normal
window after auth.

Decided in the 2026-10-10 receive-memory interview: memory allowance and
rate pacing are independent bounds. This replaces the earlier assumption
that a one-second rate window can also be the complete assembly allowance.
