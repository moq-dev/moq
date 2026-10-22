# [M] Bound the state and work a peer's counts can choose

## Goal

Every count a peer sends that allocates or scans is bounded by what the peer actually sent, or by an existing budget, rather than by the number itself:

- `SUBSCRIBE_NAMESPACE` retains no more namespaces than the session's announce budget (`rs/moq-net/src/ietf/subscriber.rs`, the per-stream `live` map).
- `ANNOUNCE_INIT` cannot turn a message body into a much larger allocation (`rs/moq-net/src/lite/announce.rs`).
- A WebSocket upgrade buffers no more than one qmux record before SETUP (`rs/moq-relay/src/websocket.rs`).
- Public HTTP admission is bounded per connection and per request (`rs/moq-relay/src/web.rs`, `rs/moq-relay/src/auth.rs`).

The `AUTH_OK` pattern reduction is already done in [#6356](https://github.com/moq-dev/moq/pull/6356) and is not repeated here.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- ✅ The `AUTH_OK` fix reduces in one pass, with no new pattern cap (#6356).
- A namespace interest takes its `announces` permit before the decision, as moq-lite already does, and releases it only when the stream ends. A reflected or refused namespace therefore holds the same permit every other announce does.
- `ANNOUNCE_INIT` is a legacy lite-01/02 message. Make its decoder take the session's announce budget, or otherwise bound the count by the body and that budget, so the existing resource limit is the bound rather than a new constant.
- The WebSocket socket caps message and frame size at qmux's own `max_record_size`, from one shared constant, since qmux carries one record per message.
- Bound admission with backpressure, not load shedding: the h2 listener sets `max_concurrent_streams`, and any global bound on in-flight admissions stops accepting or polling new work while full rather than answering 503. A valid request in an ordinary burst waits; it is never refused by a threshold, and what it holds while waiting stays within the connection and stream bounds.

Each fix lands with a regression test that fails on main. The admission fan-out gets a benchmark swept over connections and streams, per the fan-out rule.

Public API: none. Wire: none.

## Related

- [In-band auth](/quest/m1/auth/README.md) - the `AUTH_OK` decoders this area started from
- [Session limits](/quest/m1/api-audit/session-limits.md) - keeps or drops the per-session request caps these sit beside
