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

### Carried changes

Changes on top of the upstream commit, besides the renames:

- **BBR3**, from [quinn#2481](https://github.com/quinn-rs/quinn/pull/2481) at [`55f74c0d`](https://github.com/quinn-rs/quinn/pull/2481/commits/55f74c0dd0738b5ad7d7676c0b2869a547783f83) (open upstream), rebased onto the fork point with authorship kept. It replaces quinn's BBR and is the default controller. It carries the BBR correctness fixes moq-dev/noq shipped in `moq-noq` 1.3.1, and with them the `Controller` changes BBR needs: packets named by number and `SpaceId`, `on_packet_sent`, `on_packet_lost`, `on_cwnd_limited`, `on_app_limited`, `on_ack_frequency_update`, and a `pacing_rate` and `send_quantum` the pacer obeys. `ControllerMetrics` rates are bytes per second.
- **Classic ECN for BBR3**, ported from [moq-dev/noq#12](https://github.com/moq-dev/noq/pull/12): CE exits Startup, stops a bandwidth probe, or lowers the short-term model, once per recovery episode, instead of counting as a loss.
- **Pacer rounding**: a wait shorter than a nanosecond rounds up instead of re-arming the pacing timer at the current instant.
- **qlog pacing rate** in bits per second, as qlog defines it.
- moq-dev/noq's congestion-callback regressions, reworked onto quinn's test harness.
- [quinn#2724](https://github.com/quinn-rs/quinn/pull/2724) (`moq_sock::udp`): when the kernel rejects a GSO batch with `EIO` or `EINVAL`, the socket halts GSO and resends the batch as individual datagrams instead of dropping it.
  We extend it to resend batches built before GSO was halted too, which upstream's version drops, and to log only the first rejection.
  A `WouldBlock` partway through the resend makes the caller retry the whole batch, duplicating the datagrams already sent; QUIC drops the duplicates.
  Drop it if upstream lands [quinn#2748](https://github.com/quinn-rs/quinn/pull/2748) and we cherry-pick that.
- Apple fast-path fallback preserves the segmentation of batches prepared before the fast path was disabled, including when a private symbol is unavailable.

Recheck the BBR3 changes when cherry-picking quinn#2481 updates, or if it merges.

### Advisory triage

`cargo audit` cannot match the renamed crate, so security fixes are tracked by hand.
Watch quinn-rs/quinn's [security advisories](https://github.com/quinn-rs/quinn/security/advisories) and releases (including the `0.11.x` branch, which sometimes gets a fix `main` does not need).
For each quinn-proto or quinn-udp advisory, check whether the vulnerable code exists in this fork, and if it does, port the fix together with its regression test.
