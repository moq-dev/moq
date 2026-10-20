# [M] An async QUIC driver without moq-net

## Goal

An application runs `moq-quic` on tokio without depending on moq-net: the
endpoint, connection, and stream futures in `moq_tokio::quic::driver` today,
with send groups, `reset_at`, receive leases, and live `Limits`. Any Rust MoQ
implementation, or any QUIC application, can embed the stack moq-dev ships,
instead of forking quinn's async layer again. moq-tokio keeps its API by
re-exporting the driver.

`cargo tree -e normal` on the driver shows no moq-net, kio-based model, or
moq-lite code, and no crate from quinn, noq, or moq-dev/web-transport. The
driver's tests move with it and stay in the merge gate.

## Plan

Where it stands:

- The driver is quinn's async layer on moq-quic, kept as moq-tokio modules,
  tokio-only (decided 2026-10-06 in the fork epic, over `moq-quic-udp` and
  `moq-quic-tokio` crates). The udp half is now `moq_sock::udp`.
- Its only moq-net type is `moq_net::transport::Wire`, a pair of byte
  counters (`quic/driver/connection.rs`). `quic/backend.rs` also takes
  `moq_net::Versions` for ALPN lists, but sits above the driver.
- moq-net is a required dependency of moq-tokio, even without the `quic`
  feature that gates the driver, so a consumer cannot take the driver alone.
- The driver is public but `#[doc(hidden)]`, and is not on `release`.

Decided 2026-10-20 (maintainer): `moq-quic` powers qmux and WebTransport,
and moq takes no new work in, or dependency on, moq-dev/web-transport, noq
or quinn. The driver is that stack's tokio runtime, beside moq-uring's
driver and the planned in-tree qmux, so this quest moves in-tree code only.
quinn stays the upstream the fork cherry-picks from, not a dependency.

Decided 2026-10-20: m2. The cut ships the driver hidden inside moq-tokio, so
options 1 and 2 add API later without breaking anyone. Option 3 makes
moq-tokio's moq-net API optional, which breaks `default-features = false`
users, so choosing it moves this quest to m1.

**Provisional, until the maintainer decides: option 1, named `moq-quic-tokio`.**

1. **A `moq-quic-tokio` crate.** The driver moves out of moq-tokio unchanged,
   and moq-tokio depends on it and re-exports `quic::driver`. It keeps one
   sans-IO core with a crate per runtime around it, as the planned in-tree
   `qmux` crate does for its tokio session. Reverses the 2026-10-06 crate decision for the async layer
   only; quinn-udp stays a moq-sock module. A trial extraction moved the
   module as-is: once `Wire` became driver-owned counters, the driver's tests
   passed unchanged. What the move has to settle:
   - The crate depends on moq-quic, `moq_sock::udp`, and moq-clock with its
     `runtime` feature, since the connection's timer is `moq_clock::Sleep`.
   - `runtime::send_failed` and `wrap_udp_socket` are `pub(crate)` helpers
     that `quic::backend` and `quic::web`'s tests call, so they become
     public API or move.
   - Shorter paths let rustfmt re-wrap about 50 lines of `tests.rs`. Keep
     that churn out of the move commit, so quinn cherry-picks still apply.
2. **A `tokio` feature on moq-quic.** No new crate. But it puts a runtime
   into the sans-IO core that moq-uring and qmux share, and the gate covers
   the driver's logic, not just a dependency, so its tests leave the default
   merge gate, against `rs/AGENTS.md`.
3. **Keep it in moq-tokio, with moq-net behind a default feature.** The
   smallest move, but every moq-net use in moq-tokio needs a gate, and a
   driver-only build keeps the `quic` feature's other dependencies, such as
   reqwest and notify.

In every option:

- The first commit replaces `Wire` with counters owned by the driver.
  moq-tokio converts at its boundary, and moq-net's wire stats do not change.
- `quic::web` and `quic::backend` stay in moq-tokio, and
  [Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md) proceeds
  as planned on top. The driver becomes documented public API. The driver
  API changes that quest makes, such as aligning the stream setters with
  moq-uring's, land wherever the driver lives; neither quest waits on the
  other.
- moq-uring's driver is out of scope. It is `!Send`, and coupled to moq-net
  through `server::Activity` as well as `Wire`.
- Land after the receive-lease PR
  ([moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)), which
  rewrites the driver's receive path.
- Publish the driver crate, if option 1, with moq-quic, moq-sock and moq-clock
  in the same release.
