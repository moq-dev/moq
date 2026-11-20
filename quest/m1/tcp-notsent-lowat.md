# [S] QMux over TCP holds unsent data in the session, not the kernel

## Goal

Every TCP socket that carries a QMux session (`tcp://`, `tls://`, `ws://`,
`wss://`, on the relay and on native clients) sets `TCP_NOTSENT_LOWAT`, so the
kernel queues about one congestion window of data it has not yet sent. The rest stays in
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
  length, so cap each write the driver makes (`BATCH_BYTES`, 64 KiB in
  `rs/qmux/src/driver.rs`) at the mark.
- Decided 2026-11-20 (maintainer, on
  [moq-dev/moq#9220](https://github.com/moq-dev/moq/pull/9220)): ✅ the mark is
  at least the congestion window (rejected: a fixed 16 KiB). A smaller mark
  leaves the kernel with less unsent data than one window's ACKs free, so
  throughput would depend on how fast the session refills. One window of
  committed data is the most the kernel can use within a round trip, so it
  bounds stale bytes to about one RTT of media.
- The window moves, so the mark follows it: set it from `TCP_INFO` (`tcpi_snd_cwnd`
  times `tcpi_snd_mss` on Linux, `tcpi_snd_cwnd` bytes on macOS) when the socket
  is adopted, and re-read it whenever a write returns `EAGAIN`, raising or
  lowering the mark before parking. A raised mark makes the socket writable
  again at once. Reuse `TcpStats`' `TCP_INFO` read (`rs/qmux/src/socket.rs`).
- Measure before and after: bytes of a reset stream that still reach the peer
  over a bufferbloated path (a netns veth with a deep `tbf` queue, which
  `unshare -rn` sets up without root), throughput on a high-BDP path against
  no mark, and relay CPU per delivered frame on `just bench`'s `tcp` and `ws`
  cases, since the extra `getsockopt` and wakeups cost something. Record all
  three in `## Validation`.
- No config knob: the window sets the mark.

m1: additive, with no API or wire change, and it ships with the next cut.

## Related

- [WebSocket rate caps](/quest/m3/rate-websocket.md) - also bounds kernel buffers on capped sessions
