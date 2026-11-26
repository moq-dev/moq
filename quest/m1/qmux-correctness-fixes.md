# [M] Fix Rust and JS QMux correctness bugs and port web-transport fixes

## Goal

All verified Rust (`moq-quic` mux, `rs/qmux`) and JS (`js/qmux`) QMux correctness and interop bugs from the QMux audit, plus the three unported `moq-dev/web-transport` fixes (shared WS/TLS handshake timeout, `TcpStats` teardown fd release, and cached `rustls-platform-verifier` for default `wss://` roots), are fixed with targeted regression and cross-language interop tests.

Boundaries and non-goals: additive fixes only (no wire format breaks and no breaking public API changes; `qmux::ws::Server::accept` stays generic-only with no `_with_xxx` methods). Items already tracked in [quic-stream-and-wire-fixes](/quest/m1/quic-stream-and-wire-fixes.md) (mux close underflow, 32-bit `frame.rs`/`packet.rs` varint truncation) and [wire-and-transport-polish](/quest/m3/wire-and-transport-polish.md) (`config.rs` rate-cap checks, `ws.rs` stale docs) are out of scope here.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the QMux audit and `moq-dev/web-transport` check):

- Goal: ✅ Confirm full scope across correctness, `web-transport` ports, consolidation/verification, and TCP/JS perf (rejected: Correctness and web-transport ports only; Correctness and consolidation, skip perf; Drop). Joins `m1` because every fix here is additive and closes live correctness and interop bugs before the next release cut.
- Default `wss://` TLS roots in `qmux::ws::Client`: ✅ Switch to cached `rustls-platform-verifier`, matching `moq-tokio`, `moq-auth`, and [moq-dev/web-transport#512](https://github.com/moq-dev/web-transport/pull/512) (rejected: Keep `rustls-native-certs` and cache it in a `OnceLock`).
- `qmux::ws::Server::accept` TCP stats API: ✅ Leave `ws::Server::accept` generic-only (callers use `ws::Upgraded` for TCP); `_with_xxx` API methods are banned (rejected: Add `ws::Server::accept_tcp` and `accept_with_stats`).

Caused by: [moq-dev/moq#6231](https://github.com/moq-dev/moq/pull/6231) (`mux::Timer`, `mux/mod.rs`, `mux/params.rs`, `rs/qmux/src/driver.rs`, `rs/qmux/src/ws.rs`), [moq-dev/moq#8660](https://github.com/moq-dev/moq/pull/8660) (`transmitted()` before `poll_flush`), [moq-dev/moq#9242](https://github.com/moq-dev/moq/pull/9242) (`TcpStats` duplicate fd retained across teardown), [moq-dev/moq#6124](https://github.com/moq-dev/moq/pull/6124) and [moq-dev/moq#9343](https://github.com/moq-dev/moq/pull/9343) (`js/qmux`).

### Rust (`rs/moq-quic/src/connection/mux/` and `rs/qmux/`)

1. **One-way-sender idle timeout (`mux/timer.rs:116-139`)**: Gate keep-alive `QX_PING` on `last_recv.max(last_ping) + idle / 3` instead of `last_send`, so a healthy low-bitrate one-way publisher below the peer's 50% `MAX_DATA` replenishment threshold still probes and survives `idle_timeout` even with `rtt_interval: None`.
2. **Ordered `RESET_STREAM_AT` (`mux/mod.rs:615-626`, `streams/send.rs:136`)**: On QMux's ordered byte stream, hold `RESET_STREAM_AT` until `0..reliable_size` has been serialized so it is never emitted ahead of its reliable prefix (which `js/qmux` rejects as a `PROTOCOL_VIOLATION`).
3. **Flush-confirmed `transmitted()` and `reset_acked()` (`driver.rs:332-336`, `mux/mod.rs:618-622`)**: Confirm records (`transmitted()` and pending `reset_acked()`) only once `poll_flush()` drains them to the kernel/transport, not right after `send()` appends to user-space buffers.
4. **Offered `Sec-WebSocket-Protocol` rejection test (`ws.rs:308-320`)**: Pinned `tungstenite` 0.30 already validates that the server's `Sec-WebSocket-Protocol` was offered by the client; add a regression test at the `qmux::ws::Client::connect` boundary confirming an unoffered server subprotocol is rejected.
5. **32-bit overflow / truncation (`transport/stream.rs:72-112`, `mux/mod.rs:684`)**: Use checked arithmetic for `prefix + size` and `usize::try_from` instead of `as usize` truncation on peer record sizes.
6. **`WsTransport::poll_ping` waker clobbering (`transport/websocket.rs:104`, `driver.rs:271-359`)**: Stop registering `recv.waiter` on `self.ws.poll_ready` where `driver.rs` overwrites it with the main waiter, so write-backpressured keep-alive pings resume promptly.
7. **Other `mux` / `qmux` protocol gaps**:
   - Reject duplicate `MAX_RECORD_SIZE` and `APPLICATION_PROTOCOLS` with `TRANSPORT_PARAMETER_ERROR` (`mux/params.rs:121-147`) and stop running QUIC-only checks (`min_ack_delay`, receive timestamps) on QMux parameter blobs.
   - Respect remaining record budget in `encode_transport_parameters` and `encode_ping` (`mux/mod.rs:584-613`).
   - Call `wake_transmit()` when `handle_timeout` sets `ping_request` (`mux/mod.rs:420-422`).
   - Check `conn.is_closed()` in `send_datagram` so datagrams are not queued during `Phase::Flushing` (`session.rs:339-350`).

### Ports from `moq-dev/web-transport`

8. **Shared `handshake_timeout` across WS/TLS upgrade and transport parameters** ([moq-dev/web-transport#482](https://github.com/moq-dev/web-transport/pull/482), [moq-dev/web-transport#498](https://github.com/moq-dev/web-transport/pull/498)): Bound `tokio_tungstenite` (`ws.rs:303, 490`) and `tokio_rustls` (`tls.rs:95, 168`) handshakes plus `Session::start` under one shared `handshake_timeout` deadline, and validate `max_record_size` before completing the upgrade so invalid config fails locally without advertising invalid parameters.
9. **Release `TcpStats` duplicate fd at driver teardown** ([moq-dev/web-transport#481](https://github.com/moq-dev/web-transport/pull/481)): Snapshot final wire counters/RTT and release the duplicate `OwnedFd` in `TcpStats` when the driver finishes or drops (not only `shutdown(SHUT_RDWR)`), so retained `Session` clones cannot leak descriptors.
10. **Cached `rustls-platform-verifier` for default `wss://` roots** ([moq-dev/web-transport#512](https://github.com/moq-dev/web-transport/pull/512)): Replace the synchronous per-dial `rustls_native_certs::load_native_certs()` in `ws.rs:282-300` with a `OnceLock`-cached `rustls::ClientConfig` built via `rustls-platform-verifier` (already in the workspace for `moq-tokio` and `moq-auth`).

### JavaScript (`js/qmux/`)

11. **One-way-sender idle timeout (`session.ts:1264-1302`)**: Gate keep-alive `QX_PING` on `#lastRecvAt` (max with last ping time) + `idle / 3`, not `#lastSendAt`.
12. **Protocol divergences**:
    - Reject `QX_PING` on `qmux-00` (`session.ts:1119-1134`).
    - Reject duplicate `QX_TRANSPORT_PARAMETERS` on `qmux-00/01` (`session.ts:1147`).
    - Stop decoding a record in `Frame.decodeRecord()` as soon as `APPLICATION_CLOSE` or `CONNECTION_CLOSE` is decoded (matching Rust), so trailing frames or malformed trailing bytes after a valid close never hide the peer's close code and reason (`frame.ts:705`, `session.ts:1027-1030`).
    - Prevent an out-of-order `ping_request` on `qmux-01` from overwriting a higher queued `ping_response` sequence (`session.ts:1128`).
    - Return `FINAL_SIZE_ERROR` on `STREAM` data or mismatched `RESET_STREAM` after `FIN` in `sizeKnown` instead of silently dropping (`recv.ts:142-179`).
    - Clamp `StreamLimit.#raise` to `2^60` (`credit.ts:259`), and clamp huge peer `maxIdleTimeout` values so they cannot overflow JS's 32-bit `setTimeout` (`session.ts:1225`).

### Verification

- Unit tests with mocked time for every fix on both sides (including one-way low-bitrate sender staying alive past `idle_timeout` with `rtt_interval: None`, flush-gated `poll_acked`/`poll_closed`, WS subprotocol mismatch rejection, shared WS/TLS handshake timeout, and fd release after driver exit with live `Session` clones).
- Rust-to-JS `qmux-02` interop test covering `RESET_STREAM_AT` with buffered reliable prefix, plus `just test interop --all`.

Public API: none. Wire: none.

## Related

- [Session keep-alive with a stale signal](/quest/m1/keep-alive.md) - touches the same QMux timer state machine
- [Fix moq-quic stream, Assembler, QMux, BloomTokenLog, CidState, rate-cap, and 32-bit wire bugs](/quest/m1/quic-stream-and-wire-fixes.md) - owns mux close underflow and 32-bit `frame.rs`/`packet.rs` varint fixes
- [Polish wire, transport, qmux config, draft, and HLS escaping edge cases](/quest/m3/wire-and-transport-polish.md) - owns `rs/qmux/src/config.rs` rate-cap validation and `ws.rs` stale docs
