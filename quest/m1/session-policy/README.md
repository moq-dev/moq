# Session policy lives in moq-net

## Goal

Session policy lives once, in moq-net. Accept and connect deadlines (spanning
SETUP, admission and the reply, closing with Timeout), the admission bound,
widening the receive window on admit, and reconnect with backoff come from
moq-net for tokio, io_uring and WebRTC sessions alike. moq-tokio keeps only
sockets, DNS, TLS, accept loops, the fallback runtime and spawning. The epic is
done when moq-relay's hand-copied policy in `uring.rs` is deleted in favour of
the moq-net calls, and moq-webrtc's session runs through the same policy.

## Plan

Planned 2026-11-04 with the maintainer, from the WebRTC consolidation draft
[moq-dev/moq#7804](https://github.com/moq-dev/moq/pull/7804), which asked
where moq-webrtc should plug its externally established transport in. The
maintainer asked what that means for io_uring and how much of moq-tokio belongs
in moq-net, and decided:

- ✅ **Move the policy first**, then give moq-tokio thin handoffs; #7804 waits
  on [moq-tokio becomes a thin shell](/quest/m1/session-policy/tokio-handoff.md).
  Not chosen: a moq-tokio handoff now with the move later; moq-webrtc calling
  moq-net directly with its own policy.
- ✅ **Reconnect is in scope**: the reconnect, backoff and GOAWAY candidate
  walk move too, as a poll machine. Not chosen: leaving them in moq-tokio
  until io_uring dials.
- ✅ **Fold** the m2 relay-serve-path quest into
  [the io_uring path](/quest/m1/session-policy/uring.md) and the
  poll-first server-machines quest into
  [accept policy](/quest/m1/session-policy/accept.md).
- ✅ **Break moq-net's public API in m1**, before the cut, with MIGRATE notes.
  Not chosen: additive APIs beside the old ones.

Research (2026-11-04, `origin/main` at e6be13d5f8):

- moq-tokio owns the accept deadline (`server.rs` `Setup::deadline`,
  `within`, `Deadline::bound`, default 10 s in `listen.rs`), the connect
  deadline (`client.rs`, default 30 s in `connect.rs`), receive-window holding
  and widening on admit (`Request::ok`, `qmux_limits`), `Request`
  construction per transport, and reconnect (`connection.rs` `Backoff`,
  `Goaway`, `run`). All of it runs on `kio::clock`, so none of it needs tokio.
  Only sockets, DNS, Happy Eyeballs, TLS, accept loops, `runtime::scope`,
  spawning and WebRTC's own UDP socket are tokio-specific.
- moq-relay's io_uring path re-implements the server side by hand
  (`rs/moq-relay/src/uring.rs`: its own `Deadline`, the admission bound,
  `with_deadline`, `set_limits`, the drain-bounded reply). Eight commits
  patched drift between the copies (58eaa6afd5, 8a13d58910, 6bd1306d6e,
  1f56330d21, d17b84ee7f, 4576bb1231, c11192c7e6, 67df70012d). moq-uring
  itself has no client side.
- Widening on admit needs a transport hook: moq-net's `transport::poll`
  session exposes only `receive_window()`, while tokio QUIC, qmux and uring's
  `web::Session` all have `set_limits`. Add a trait method whose default
  reports unsupported, the same move [QUIC deadline](/quest/m2/quic-deadline.md)
  plans for per-stream deadlines.
- moq-net stays poll-first and spawns nothing
  ([Libraries spawn nothing](https://github.com/moq-dev/moq/pull/7805)):
  policy returns drivers and machines, and the caller spawns and does the I/O.

## Required

- [Accept policy in moq-net](/quest/m1/session-policy/accept.md) - one accept deadline, admission bound and window widening for every transport
- [Connect policy in moq-net](/quest/m1/session-policy/connect.md) - the connect deadline and reconnect as a poll machine
- [The io_uring relay path uses moq-net's policy](/quest/m1/session-policy/uring.md) - deletes `uring.rs`'s copy
- [moq-tokio becomes a thin shell](/quest/m1/session-policy/tokio-handoff.md) - sockets, TLS, accept loops and spawning, plus the transport handoffs WebRTC needs
