# [S] QMux over TCP holds unsent data in the session, not the kernel

## Goal

Every TCP socket that carries a QMux session (`tcp://`, `tls://`, `ws://`,
`wss://`, on the relay and on native clients) sets `TCP_NOTSENT_LOWAT`, so the
kernel queues only a small amount of data it has not yet sent. The rest stays in
the session's stream buffers, where a reset or expired stream drops it before it
reaches the wire. Today the kernel autotunes the send buffer to several MiB, which
commits seconds of stale media on a slow viewer.

Non-goals: choosing the kernel's congestion controller, and anything already sent,
which TCP must retransmit.

## Plan

Decided 2026-11-19 (maintainer, /quest-plan of user-space TCP): ✅ the kernel
step first, as the baseline the [user-space TCP epic](/quest/m4/userspace-tcp/README.md)
must beat (rejected: user-space TCP only; this quest only).

- Set the option wherever a QMux socket is prepared: `qmux::tcp::adopt`
  (`rs/qmux/src/tcp.rs`) and each site that sets `set_nodelay` on a socket
  that later carries a WebSocket session (`rs/moq-relay/src/listener.rs`,
  `rs/moq-relay/src/web.rs`, `rs/moq-tokio/src/websocket.rs`). Linux and macOS both
  support it; other platforms skip it.
- On Linux, `sendmsg` returns `EAGAIN` once unsent bytes reach the mark, so
  tokio's write-first loop honors it. One write can still overshoot by its own
  length, so the driver's batch (`BATCH_BYTES`, 64 KiB in
  `rs/qmux/src/driver.rs`) bounds the overshoot: shrink the write when the mark
  is set.
- Start at 16 KiB (Cloudflare's HTTP/2 prioritization choice). Measure it
  before fixing the value: bytes of a reset stream that still reach the peer,
  over a bufferbloated path (a netns veth with a deep `tbf` queue, which
  `unshare -rn` sets up without root), and relay CPU per delivered frame on
  `just bench`'s `tcp` and `ws` cases, since a lower mark means more wakeups.
  Record both in `## Validation`.
- No config knob. A constant that callers need to tune is a reason to revisit,
  not a starting point.

m1: additive, with no API or wire change, and it ships with the next cut.

## Related

- [WebSocket rate caps](/quest/m3/rate-websocket.md) - also bounds kernel buffers on capped sessions
