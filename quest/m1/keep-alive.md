# [M] Session keep-alive with a stale signal

## Goal

A session opened with `keep_alive: { soft, hard }`, such as 2 s and 10 s,
learns when its peer goes quiet without closing on it. Both Rust and JS
take it as an option on connect and accept.

- The library sends a QUIC PING only when nothing is in flight, so a busy
  session pays nothing for it.
- The session exposes a watch that turns `Stale` once `soft` passes with no
  reply from the peer, and turns `Live` again on the next reply.
- `hard` closes the session, taking the idle timeout's place.

A session without the option behaves as today. Its keep-alive is derived
from the negotiated idle timeout (moq-dev/moq#7347).

## Plan

Decided 2026-11-25 (maintainer, /quest-plan via kixel-ai/kixel.ai):

- ✅ A per-session option (rejected: one client- or server-wide config, which
  can't vary per link).
- ✅ Planned upstream in moq, since it is generic (rejected: drop it). It
  serves redundant ingest, where a publisher fails over to its second
  ingest path when the first goes stale. It also serves Kixel's routing by
  link quality.
- ✅ The design is the maintainer's: "keep-alive(min: 2s, max: 10s) would
  notify us when the peer hasn't replied in 2s and is terminal at 10s; the
  QUIC library would automatically PING if needed".
- ✅ m1, since it is additive.

This reverses one rejection in [moq-dev/moq#7347](https://github.com/moq-dev/moq/pull/7347)'s
quest (`git show 2a743c82b7^:quest/m2/quic-keep-alive.md`). That quest
rejected a live per-connection keep-alive for want of a consumer, and this
is one.

Guidance:

- A reply is any packet the peer sends. Send a PING once `soft / 2` passes
  with nothing heard and nothing ack-eliciting in flight, so a lost probe
  still leaves time for a second before `soft`. Drive both thresholds from
  the connection's existing timers in `moq-quic`, not from a loop of moq's
  own.
- Enforce `hard` in the session itself. On accept, the server's transport
  parameters go out before the application picks the option, so it cannot
  shorten the negotiated `max_idle_timeout`. Closing after `hard` silent
  works on both sides. The idle timeout stays as a backstop, and a `hard`
  longer than it is refused at setup.
- The transport trait (`rs/moq-net/src/transport.rs`) gains what this needs.
  A backend that can't ping or report a reply refuses the option when the
  session opens, so it never passes silently. That covers moq-tokio and
  moq-uring (on `moq-quic`), iroh (upstream noq), qmux, and moq-webrtc. qmux
  already pings over WebSocket (`qmux::ws::KeepAlive`) and can serve the
  same signal.
- `Stale` is session-wide evidence, never a track's: a quiet track on a live
  session stays `Live`. [Route health](/quest/m3/route-health/README.md)
  keeps judging tracks by comparison, and a node can use `Stale` to demote a
  whole link sooner.
- `moq-ffi` waits for a binding consumer.

Test it on the sim with bufferbloat: a link that blackholes turns `Stale`
within `soft`, and comes back `Live` when it heals before `hard`. One that
stays dead closes at `hard`. A busy session sends no PINGs, and an idle one
sends about one per `soft / 2`.

## Waiting

- Maintainer: what JS does over WebTransport, where the browser API has no
  PING and no "last packet received". (`getStats()` reports
  `packetsReceived` only in some browsers, and only by polling.) Options:
  (a) JS takes the option over qmux, which pings on its own, and refuses it
  on WebTransport at connect (recommended: it fails loud, and a native QUIC
  stack is the one place that can probe without a wire change); (b) a
  moq-lite PING/PONG control message on lite-08-wip, so every transport
  works the same, at the cost of a wire change and of probing above QUIC's
  loss recovery; (c) poll `getStats()` where it is available, which breaks
  moq's rule against timer loops and misses the browsers that lack it.

## Related

- [Route health](/quest/m3/route-health/README.md) - per-track failover, which never relies on QUIC keep-alives
- [JS session cost](/quest/m1/js-session-cost.md) - Kixel's other link-quality primitive
