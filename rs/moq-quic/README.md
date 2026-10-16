# moq-quic

MoQ's sans-IO QUIC state machine, a hard fork of [quinn-proto](https://github.com/quinn-rs/quinn).

All credit for the code goes to the quinn developers; it stays under quinn's MIT or Apache-2.0 license.

## Upstream

Forked from quinn-proto in quinn-rs/quinn `main` at [`7616e6b2`](https://github.com/quinn-rs/quinn/commit/7616e6b2782722f3ce4a1b181ef08817d7f545e4) (2026-10-05).
That covers every quinn security advisory published through 2026-09-30.

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
  | sed -E 's#([ab])/quinn/src/lib\.rs#\1/rs/moq-tokio/src/quic/driver/mod.rs#g; s#([ab])/quinn/src/#\1/rs/moq-tokio/src/quic/driver/#g; s/\bcrate::/crate::quic::driver::/g; s/\bproto::/moq_quic::/g; s/\budp::/moq_sock::udp::/g' \
  | git am -3
```

The driver is tokio-only: the `Runtime` trait, smol, the `futures-io` impls, and the lock-tracking mutex are dropped, so hunks touching them are skipped.
moq-tokio's `quic::web` module is `web-transport-moq` from moq-dev/noq at [`e27c0ce0`](https://github.com/moq-dev/noq/commit/e27c0ce00741d81c8620b98e345d4f2c88e0768c), which has no upstream left to track.

### Carried changes

Changes on top of the upstream commit, besides the renames:

- **BBR3**, from [quinn#2481](https://github.com/quinn-rs/quinn/pull/2481) at [`55f74c0d`](https://github.com/quinn-rs/quinn/pull/2481/commits/55f74c0dd0738b5ad7d7676c0b2869a547783f83) (open upstream), rebased onto the fork point with authorship kept. It replaces quinn's BBR and is the default controller. It carries the BBR correctness fixes moq-dev/noq shipped in `moq-noq` 1.3.1, and with them the `Controller` changes BBR needs: packets named by number and `SpaceId`, `on_packet_sent`, `on_packet_lost`, `on_cwnd_limited`, `on_app_limited`, `on_ack_frequency_update`, and a `pacing_rate` and `send_quantum` the pacer obeys. `ControllerMetrics` rates are bytes per second.
- **BBR3 packet retirement**: per-space packet lookup and round-indexed expiry retire completed snapshots incrementally, preserving the extra ACK batch of metadata for late loss callbacks. The bounded indexes handle sparse packet numbers without walking the live flight. Upstream submission is deferred while quinn#2481 remains open: this fork also carries the packet-space and loss callback adaptations, so a port needs to target that implementation after its shape settles.
- **Classic ECN for BBR3**, ported from [moq-dev/noq#12](https://github.com/moq-dev/noq/pull/12): CE exits Startup, stops a bandwidth probe, or lowers the short-term model, once per recovery episode, instead of counting as a loss.
- **Pacer rounding**: a wait shorter than a nanosecond rounds up instead of re-arming the pacing timer at the current instant.
- **Malformed control input**: `RETIRE_CONNECTION_ID` for a sequence that was never issued (`issued` is a count, so `sequence >= issued`) and a repeated `grease_quic_bit` or `min_ack_delay` transport parameter close the connection. RFC 9000 section 7.4 only says SHOULD for a duplicate parameter; this fork refuses it, as it does every other known parameter.
- **qlog pacing rate** in bits per second, as qlog defines it.
- moq-dev/noq's congestion-callback regressions, reworked onto quinn's test harness.
- [quinn#2724](https://github.com/quinn-rs/quinn/pull/2724) (`moq_sock::udp`): when the kernel rejects a GSO batch with `EIO` or `EINVAL`, the socket halts GSO and resends the batch as individual datagrams instead of dropping it.
  We extend it to resend batches built before GSO was halted too, which upstream's version drops, and to log only the first rejection.
  A `WouldBlock` partway through the resend makes the caller retry the whole batch, duplicating the datagrams already sent; QUIC drops the duplicates.
  Drop it if upstream lands [quinn#2748](https://github.com/quinn-rs/quinn/pull/2748) and we cherry-pick that.
- **Unreachable sends** (`moq_sock::udp::is_unreachable_err`, the driver's `UdpSenderHelper`): `ENETUNREACH`, `EHOSTUNREACH` and `ENETDOWN` reach the connection instead of being logged as loss. A handshaking connection fails at once with `NO_VIABLE_PATH`, so a dial race moves to the next address; an established one still counts them as loss. Addresses [quinn#2766](https://github.com/quinn-rs/quinn/issues/2766), reworked from [kixelated/quinn#3](https://github.com/kixelated/quinn/pull/3).
- Apple fast-path fallback preserves the segmentation of batches prepared before the fast path was disabled, including when a private symbol is unavailable.
- Discarding Initial or Handshake keys resets the PTO backoff, as RFC 9002 A.11 does, so a client's Initial backoff no longer delays the retransmission of a lost Finished. Also [moq-dev/noq#32](https://github.com/moq-dev/noq/pull/32); not offered upstream yet.
- `TransportConfig::handshake_idle_timeout` (default 10s, as msquic's `HandshakeIdleTimeoutMs`) governs the idle timer until the handshake completes, and the negotiated idle timeout takes over once established. A 2s idle timeout otherwise expires 3x the initial PTO in, just before the second Initial probe, leaving a client two flights. A server also drops an Initial left waiting for `accept` past it, and a deadline too far out to represent never fires instead of panicking. Also [moq-dev/noq#34](https://github.com/moq-dev/noq/pull/34), whose third change (the handshake idle timeout picks noq's idle-based PTO interval cap) has nothing to apply to here, since quinn's PTO backoff has no such cap.
  quinn's `handshake_timeout` test in its async layer expects the handshake to end at the idle timeout, so the driver's copy sets both timeouts.
- **Instants on [moq-clock](../moq-clock)**: the crate's `Instant` is `moq_clock::Instant`, so one clock steps every timer, tokio's paused clock and the simulator included. qlog stamps events relative to `QlogConfig::start_time` on that clock rather than the streamer's own `std::time::Instant`.
- **`RESET_STREAM_AT`** ([draft-ietf-quic-reliable-stream-reset](https://datatracker.ietf.org/doc/draft-ietf-quic-reliable-stream-reset/)): the `reset_stream_at` transport parameter is always advertised, `Checkpoint::reset` (from `SendStream::checkpoint`) keeps the data written before the checkpoint reliable, holding a plain `RESET_STREAM` until that data is acknowledged when the peer lacks the extension, and a received reset surfaces only once the reliable prefix has been read. Stopping a stream after it was reset no longer credits its unread data twice.
- **Draining endpoint event**: `EndpointEvent::is_draining` fires once per connection when it stops being active, before the drained event, so the driver's `Endpoint::wait_all_draining` can end a shutdown without sitting out every closing period.

Recheck the BBR3 changes when cherry-picking quinn#2481 updates, or if it merges.

### Advisory triage

`cargo audit` cannot match the renamed crate, so security fixes are tracked by hand.
Watch quinn-rs/quinn's [security advisories](https://github.com/quinn-rs/quinn/security/advisories) and releases (including the `0.11.x` branch, which sometimes gets a fix `main` does not need).
For each quinn, quinn-proto, or quinn-udp advisory, check whether the vulnerable code exists in this fork, and if it does, port the fix together with its regression test.
