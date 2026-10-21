# moq-quic

MoQ's sans-IO QUIC state machine, a hard fork of [quinn-proto](https://github.com/quinn-rs/quinn).

All credit for the code goes to the quinn developers; it stays under quinn's MIT or Apache-2.0 license.

## Upstream

Forked from quinn-proto in quinn-rs/quinn `main` at [`7616e6b2`](https://github.com/quinn-rs/quinn/commit/7616e6b2782722f3ce4a1b181ef08817d7f545e4) (2026-10-05).
That covers every quinn security advisory published through 2026-09-30.

Cherry-picked since the fork point:

- [`48854d48`](https://github.com/quinn-rs/quinn/commit/48854d48635875d259a95973cc0575db8c1792a4): accept non-minimally encoded integer transport parameters, as RFC 9000 section 16 allows.
- [`83701a58`](https://github.com/quinn-rs/quinn/commit/83701a58d10f6cb089c48aeff45e137ebd2712ec): an ordered read refused after an unordered one leaves the stream readable instead of discarding it.
- [`38b9f52f`](https://github.com/quinn-rs/quinn/commit/38b9f52fd7b545cc2d3fa81bc2fbaed072fae0f4): drop already-read duplicates when a stream switches from ordered to unordered reads.

The fork never merges upstream.
Upstream fixes are cherry-picked by hand, and the crate keeps quinn's formatting (rustfmt defaults) so the patches apply cleanly.
Rewrite quinn's crate path and name in the patch before applying it, so its context matches the renamed lines:

```sh
git -C ../quinn format-patch -1 --stdout <sha> -- quinn-proto \
  | sed -E 's#([ab])/quinn-proto/#\1/rs/moq-quic/#g; s/quinn_proto/moq_quic/g' \
  | git am -3
```

`Cargo.toml` hunks still need applying by hand, since the manifest renames the package and inlines quinn's workspace dependency specs.

quinn-udp, from the same commit, is the `udp` module of [moq-sock](../moq-sock/README.md).
Its patches map onto the module the same way:

```sh
git -C ../quinn format-patch -1 --stdout <sha> -- quinn-udp/src quinn-udp/tests/tests.rs \
  | sed -E 's#([ab])/quinn-udp/src/lib\.rs#\1/rs/moq-sock/src/udp/mod.rs#g; s#([ab])/quinn-udp/src/#\1/rs/moq-sock/src/udp/#g; s#([ab])/quinn-udp/tests/tests\.rs#\1/rs/moq-sock/tests/udp/main.rs#g; s/crate::/crate::udp::/g; s/quinn_udp::/moq_sock::udp::/g' \
  | git am -3
```

Changes to its `Cargo.toml` and `build.rs` go into moq-sock's by hand, and its benchmark is not carried (moq-uring's `udp_tokio` covers it).
The module logs through `tracing` only, so quinn-udp's `log` feature and no-op logger are dropped.
A `.rustfmt.toml` in each imported directory keeps quinn's formatting; `cargo fmt` cannot apply it to a submodule, so moq-sock skips `udp` and `just rs fix` formats it separately.

quinn itself, the async layer from the same commit, is the `quic::driver` module of [moq-tokio](../moq-tokio), formatted the same way:

```sh
git -C ../quinn format-patch -1 --stdout <sha> -- quinn/src \
  | sed -E 's#([ab])/quinn/src/lib\.rs#\1/rs/moq-tokio/src/quic/driver/mod.rs#g; s#([ab])/quinn/src/#\1/rs/moq-tokio/src/quic/driver/#g; s/(^|[^[:alnum:]_])crate::/\1crate::quic::driver::/g; s/(^|[^[:alnum:]_])proto::/\1moq_quic::/g; s/(^|[^[:alnum:]_])udp::/\1moq_sock::udp::/g' \
  | git am -3
```

The word boundaries are spelled `(^|[^[:alnum:]_])` because BSD sed, as on macOS, has no `\b` and would silently leave the paths unrewritten.
The driver is tokio-only: the `Runtime` trait, smol, the `futures-io` impls, and the lock-tracking mutex are dropped, so hunks touching them are skipped.
moq-tokio's `quic::web` module is `web-transport-moq` from moq-dev/noq at [`e27c0ce0`](https://github.com/moq-dev/noq/commit/e27c0ce00741d81c8620b98e345d4f2c88e0768c), which has no upstream left to track.

### Carried changes

Changes on top of the upstream commit, besides the renames:

- **BBR3**, from [quinn#2481](https://github.com/quinn-rs/quinn/pull/2481) at [`55f74c0d`](https://github.com/quinn-rs/quinn/pull/2481/commits/55f74c0dd0738b5ad7d7676c0b2869a547783f83) (open upstream), rebased onto the fork point with authorship kept. It replaces quinn's BBR and is the default controller. It carries the BBR correctness fixes moq-dev/noq shipped in `moq-noq` 1.3.1, and with them the `Controller` changes BBR needs: packets named by number and `SpaceId`, `on_packet_sent`, `on_packet_lost`, `on_cwnd_limited`, `on_app_limited`, `on_ack_frequency_update`, and a `pacing_rate` and `send_quantum` the pacer obeys. `ControllerMetrics` rates are bytes per second.
- **BBR3 loss samples**: a lost packet supplies its own inflight, loss, and application-limited state without borrowing or overwriting the ACK sample, including before the first ACK observation. This follows draft-06 and Google Linux BBRv3; upstream submission waits on quinn#2481 for the same callback adaptation as packet retirement.
- **BBR3 packet retirement**: per-space packet lookup and round-indexed expiry retire completed snapshots incrementally, preserving the extra ACK batch of metadata for late loss callbacks. The bounded indexes handle sparse packet numbers without walking the live flight. Upstream submission is deferred while quinn#2481 remains open: this fork also carries the packet-space and loss callback adaptations, so a port needs to target that implementation after its shape settles.
- **Classic ECN for BBR3**, ported from [moq-dev/noq#12](https://github.com/moq-dev/noq/pull/12): CE exits Startup, stops a bandwidth probe, or lowers the short-term model, once per recovery episode, instead of counting as a loss.
- **Pacer rounding**: a wait shorter than a nanosecond rounds up instead of re-arming the pacing timer at the current instant.
- **Malformed control input**: `RETIRE_CONNECTION_ID` for a sequence that was never issued (`issued` is a count, so `sequence >= issued`) and a repeated `grease_quic_bit` or `min_ack_delay` transport parameter close the connection. RFC 9000 section 7.4 only says SHOULD for a duplicate parameter; this fork refuses it, as it does every other known parameter. A STREAM frame whose FIN sets a final size below data already received closes with `FINAL_SIZE_ERROR`, as RFC 9000 section 4.5 requires, where quinn accepts it and leaves the reader blocked forever. Not offered upstream yet.
- **`ACK_FREQUENCY` upper bound**: a Requested Max Ack Delay of 2^14 ms or more closes the connection with `PROTOCOL_VIOLATION`, as [draft-ietf-quic-ack-frequency-14](https://www.ietf.org/archive/id/draft-ietf-quic-ack-frequency-14.html#section-4) section 4 requires, mirroring RFC 9000's bound on the `max_ack_delay` transport parameter, and a configured `AckFrequencyConfig::max_ack_delay` is requested just below it. quinn checks only the lower bound; not offered upstream yet.
- **qlog pacing rate** in bits per second, as qlog defines it.
- moq-dev/noq's congestion-callback regressions, reworked onto quinn's test harness.
- [quinn#2724](https://github.com/quinn-rs/quinn/pull/2724) (`moq_sock::udp`): when the kernel rejects a GSO batch with `EIO` or `EINVAL`, the socket halts GSO and resends the batch as individual datagrams instead of dropping it.
  We extend it to resend batches built before GSO was halted too, which upstream's version drops, and to log only the first rejection.
  A `WouldBlock` partway through the resend makes the caller retry the whole batch, duplicating the datagrams already sent; QUIC drops the duplicates.
  Drop it if upstream lands [quinn#2748](https://github.com/quinn-rs/quinn/pull/2748) and we cherry-pick that.
- **Unreachable sends** (`Connection::handle_unreachable`): a driver reports a send that fails with `ENETUNREACH`, `EHOSTUNREACH` or `ENETDOWN` (`moq_sock::udp::is_unreachable_err`) instead of logging it as loss. A handshaking connection fails at once with `NO_VIABLE_PATH` and sends no CONNECTION\_CLOSE, so a dial race moves to the next address; an established one ignores it and the datagram counts as loss. Addresses [quinn#2766](https://github.com/quinn-rs/quinn/issues/2766), reworked from [kixelated/quinn#3](https://github.com/kixelated/quinn/pull/3).
- Apple fast-path fallback preserves the segmentation of batches prepared before the fast path was disabled, including when a private symbol is unavailable.
- Discarding Initial or Handshake keys resets the PTO backoff, as RFC 9002 A.11 does, so a client's Initial backoff no longer delays the retransmission of a lost Finished. Also [moq-dev/noq#32](https://github.com/moq-dev/noq/pull/32); not offered upstream yet.
- `TransportConfig::handshake_idle_timeout` (default 10s, as msquic's `HandshakeIdleTimeoutMs`) governs the idle timer until the handshake completes, and the negotiated idle timeout takes over once established. A 2s idle timeout otherwise expires 3x the initial PTO in, just before the second Initial probe, leaving a client two flights. A server also drops an Initial left waiting for `accept` past it, and a deadline too far out to represent never fires instead of panicking. Also [moq-dev/noq#34](https://github.com/moq-dev/noq/pull/34), whose third change (the handshake idle timeout picks noq's idle-based PTO interval cap) has nothing to apply to here, since quinn's PTO backoff has no such cap.
  quinn's `handshake_timeout` test in its async layer expects the handshake to end at the idle timeout, so the driver's copy sets both timeouts.
- **Instants on [moq-clock](../moq-clock)**: the crate's `Instant` is `moq_clock::Instant`, so one clock steps every timer, tokio's paused clock and the simulator included. qlog stamps events relative to `QlogConfig::start_time` on that clock rather than the streamer's own `std::time::Instant`.
- **Send groups** (`streams/schedule.rs`, `Connection::send_groups`): streams outside every group go first, in quinn's order; groups are served strictly by priority, with equal priorities sharing bytes by deficit round robin. A stream's priority is an `i64` ordering it only within its group, and `Streams::open` takes a `SendOptions { group, priority }` to start it there, like W3C WebTransport's send options. The schedule also has scheduling domains that share bytes the same way, crate-private until a consumer needs them. This replaces quinn's single pending-stream heap, so a cherry-pick touching `PendingStreamsQueue` has to be redone against the schedule. moq-tokio's `quic::driver` and moq-uring's driver wrap a group in a shared `SendGroup` handle that closes on drop.
- **Scheduled datagrams** (`Datagrams::send`): a datagram is queued by its `SendOptions`, in a send group (or ungrouped) at an `i64` priority, and scheduled with the group's streams, winning a tie with a stream of equal priority and spending the group's byte credit, instead of quinn's datagram-first FIFO in `populate_packet`. Sending never blocks: `datagram_send_buffer_size` is shared, and an overflow evicts the oldest datagram of the lowest-priority group in the domain holding the most datagram bytes. `SendDatagramError::Blocked` and `Event::DatagramsUnblocked` are gone, and a cherry-pick touching the `// DATAGRAM` loop has to be redone against `StreamsState::write_frames`.
- **Sharded connection IDs** (`RandomConnectionIdGenerator::set_shard`): every CID names shard `index` of a `count`-endpoint group in its first byte, modulo `count`, and sets a marker in its fourth byte, so a `SO_REUSEPORT` steering filter delivers a packet to the endpoint that issued its CID. `validate` refuses a CID without this shard's prefix and marker, so a misrouted packet is dropped instead of answered with a stateless reset. Not offered upstream; it serves moq-sock's per-core sharding.
- **`RESET_STREAM_AT`** ([draft-ietf-quic-reliable-stream-reset](https://datatracker.ietf.org/doc/draft-ietf-quic-reliable-stream-reset/)): the `reset_stream_at` transport parameter is always advertised, `SendStream::reset_at` keeps the data written before an offset reliable, holding a plain `RESET_STREAM` until that data is acknowledged when the peer lacks the extension, and a received reset surfaces only once the reliable prefix has been read. Stopping a stream after it was reset no longer credits its unread data twice.
- **ACK progress** (`SendStream::offset`, `SendStream::acked`, `StreamEvent::Acked`): `acked` reports whether the peer acknowledged every byte before an offset, by comparing against the stream's contiguous acknowledged prefix. A `false` stores the lowest polled offset on the stream, and the ACK that advances the prefix past it, or a reset or `STOP_SENDING` that makes it fail, reports `StreamEvent::Acked` with the prefix, so a cherry-pick touching `StreamsState::received_ack_of`, `received_stop_sending`, or the reset path has to keep that report. Not offered upstream: quinn has no API for when sent data was acknowledged.
- **Fuzzing** runs through the hidden `fuzz` module behind a `fuzz` feature, with one-line shims in [`fuzz/`](fuzz/README.md): upstream's `packet`, `params`, `streamid`, and `streams` targets, plus `frame` and a connection op sequence. Upstream's `cfg(fuzzing)` `fuzzing` module and `arbitrary` feature are gone, so its fuzz patches port by hand.
- **Draining endpoint event**: `EndpointEvent::is_draining` fires once per connection when it stops being active, before the drained event, so the driver's `Endpoint::wait_all_draining` can end a shutdown without sitting out every closing period.
- **Late stream limits**: peer transport parameters that raise a stream limit after an open was refused emit `StreamEvent::Available`, as `MAX_STREAMS` does. A server accepted before the rest of a multi-packet ClientHello (a post-quantum key share is enough) otherwise leaves a 0.5-RTT `open_uni` parked until the idle timeout. Not offered upstream yet.
- **Stream credit on stop**: `RecvStream::stop` on a stream whose final size is already known (finished or reset) frees it and queues `MAX_STREAMS` at once, as reading the reset does, instead of leaving the credit until the next packet arrives. Not offered upstream yet; quinn `main` has the same gap.
- **Drop resets** (`quic::driver::SendStream`): a send stream dropped before `finish` is reset with code 0, or with the peer's `STOP_SENDING` code, instead of quinn's implicit `finish`, so a cancelled writer never ends a truncated stream with a FIN. This matches moq-uring's driver and moq-net's transport contract. `quic::web` resets after the WebTransport header with the mapped code. quinn's `stream_stopped_2` finishes its stream before dropping it. Not offered upstream: it reverses a documented quinn behaviour.
- **Connection limits** (`Limits`, `Connection::set_limits`, and the drivers' `set_limits`): one validated call changes a live connection's stream limit and windows, refusing values a peer would reject and changing nothing then. moq-tokio and moq-uring re-export the type, so both runtimes check limits the same way. Not offered upstream: it only bundles quinn's existing setters.

Recheck the BBR3 changes when cherry-picking quinn#2481 updates, or if it merges.

### Advisory triage

`cargo audit` cannot match the renamed crate, so security fixes are tracked by hand.
Watch quinn-rs/quinn's [security advisories](https://github.com/quinn-rs/quinn/security/advisories) and releases (including the `0.11.x` branch, which sometimes gets a fix `main` does not need).
For each quinn, quinn-proto, or quinn-udp advisory, check whether the vulnerable code exists in this fork, and if it does, port the fix together with its regression test.
