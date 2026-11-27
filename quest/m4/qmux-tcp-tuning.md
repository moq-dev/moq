# [M] Optimize QMux TCP syscalls, congestion control, record sizing, and send batching

## Goal

Reduce QMux TCP and WebSocket latency, syscall overhead, mutex churn, and send-path copies on Linux and macOS, verified with before/after bufferbloat and micro-benchmark measurements, with no wire or breaking public API changes.

Boundaries and non-goals: covers kernel-socket QMux transports in `rs/qmux` and `rs/moq-quic/src/connection/mux/`; does not duplicate [userspace-tcp](/quest/m4/userspace-tcp/qmux.md) or [io_uring stream transports](/quest/m9/stream-transports/README.md). `SO_MAX_PACING_RATE` stays with [rate-websocket](/quest/m3/rate-websocket.md), and kTLS/`SEND_ZC` stay with [ring](/quest/m9/stream-transports/ring.md).

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the QMux audit):

- Kernel TCP knobs: ✅ BBR + `TCP_USER_TIMEOUT` + MSS-aware capping + `TCP_NOTSENT_LOWAT`/coalesced `TCP_INFO`/vectored writes; gate `TCP_FASTOPEN` and selective `TCP_QUICKACK` on benchmarks (rejected: Enable all including `TCP_FASTOPEN` and selective `TCP_QUICKACK` by default; Conservative only).
  - Narrowed in PR review: `TCP_USER_TIMEOUT` stays unset on live sessions and is optional during teardown only (item 4), since Linux applies it to zero-window stalls that QMux's own liveness policy must own.

Joins `m4` (behind the release cut) as performance and optimization work with no release blocker.

Work items (measure before and after on bufferbloat and micro-benchmarks):

1. **Coalesce `TCP_INFO` syscalls (`socket.rs:253-461`, `driver.rs:377-381`)**: Replace the 3-4 separate `getsockopt(TCP_INFO)` calls across `metrics()`, `read_wire()`, and `limit_unsent()` with one cached snapshot per 250ms driver tick (surfacing `min_rtt`, `retrans`, and `notsent_bytes`).
2. **Eliminate wasted keep-alive and RTT traffic (`mux/mod.rs:101`, `session.rs:388`)**: Disable 1 Hz `QX_PING` RTT probes when `SocketStats::rtt()` is present (safe once [qmux-correctness-fixes](/quest/m1/qmux-correctness-fixes.md) fixes the one-way-sender idle-timeout bug), and restrict `ws::KeepAlive` to `qmux-00` since `qmux-01/02` enforce `idle_timeout` in-band.
3. **Fix `TCP_NOTSENT_LOWAT` floor and refresh (`socket.rs:449-461`, `driver.rs:377-381`)**: Clamp Linux `TCP_NOTSENT_LOWAT` to `max(cwnd, 4 * mss)` (or 16 KiB) so congestion (`cwnd = 2 * MSS`) does not starve `EPOLLOUT`, and refresh it on the existing 250ms `TCP_INFO` read (0 extra syscalls) instead of only on stall. On macOS, add a floor (e.g. 64 KiB) to `SO_SNDBUF = 2 * cwnd` or benchmark leaving kernel autotuning intact.
4. **Best-effort Linux `TCP_CONGESTION = "bbr"` (and leave `TCP_USER_TIMEOUT` unset while live)**: Opt into `bbr` per socket on Linux when permitted by sysctl (ignoring `EPERM`/`ENOENT`). Leave `TCP_USER_TIMEOUT` unset on live sessions where inbound QMux records can extend the idle deadline while outbound TCP is zero-window stalled, so the kernel cannot preempt QMux's liveness policy (if used at all, apply `TCP_USER_TIMEOUT` only during teardown once QMux is closing).
5. **MSS-aware QMux and TLS record sizing**: Cap QMux record / TLS fragment size (e.g. `min(cwnd, 4 * mss)` when `cwnd` is small, or `rustls::Connection::set_buffer_limit`) to cut head-of-line loss tail latency past a small `TCP_NOTSENT_LOWAT`.
6. **Send batching, vectored writes, and `Assembler` sizing (`driver.rs:296-351`, `transport/stream.rs:184-194`, `mux/mod.rs:684`)**: Pull a send batch under one mutex lock and write `[varint_len, record]` via `poll_write_vectored` (eliminating `Stream::write`'s extra full-payload copy and cutting 10 lock pairs per 4-record batch). Fix over-charged `Assembler::defragment()` allocation (`limit as usize` per small `STREAM` frame).
7. **Benchmark-gated `TCP_FASTOPEN` and selective `TCP_QUICKACK`**: Measure `TCP_FASTOPEN` / `TCP_FASTOPEN_CONNECT` (1 RTT saved on reconnect/fallback) and selective `TCP_QUICKACK` (handshake / small control exchanges) under real and impaired links; enable only if benchmarks show a clear win without middlebox regressions.

Public API: none. Wire: none.

## Required

- [Fix Rust and JS QMux correctness bugs and port web-transport fixes](/quest/m1/qmux-correctness-fixes.md) - disabling 1 Hz `QX_PING` RTT probes requires the one-way-sender idle-timeout fix first

## Related

- [Redesign the QUIC I/O boundary around io_uring](/quest/m5/quic-io-boundary.md) - zero-copy transmit boundary
- [WebSocket enforces bitrate caps](/quest/m3/rate-websocket.md) - `SO_MAX_PACING_RATE` for bitrate-capped TCP/WS sessions
- [io_uring stream transports](/quest/m9/stream-transports/ring.md) - future kTLS and `SEND_ZC` fanout
