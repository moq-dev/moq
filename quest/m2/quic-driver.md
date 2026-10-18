# [M] An async QUIC driver without moq-net

## Goal

An application runs `moq-quic` on tokio without depending on moq-net: the
endpoint, connection, and stream futures in `moq_tokio::quic::driver` today,
with send groups, `reset_at`, receive leases, and live `Limits`. Any Rust MoQ
implementation, or any QUIC application, can embed the stack moq-dev ships,
instead of forking quinn's async layer again. moq-tokio keeps its API by
re-exporting the driver.

`cargo tree -e normal` on the driver shows no moq-net, kio-based model, or
moq-lite code. The driver's tests move with it and stay in the merge gate.

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
- [Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md) moves
  `quic::web` onto moq-net's `transport::poll` traits and asks whether
  `driver` and `web` should become private.

Decide before the next breaking cut, because that cut publishes moq-quic and
the driver for the first time. Moving the driver after publication costs every
consumer a second break.

Options:

1. **A `moq-quic-tokio` crate (recommended).** The driver moves out of
   moq-tokio unchanged, and moq-tokio depends on it and re-exports
   `quic::driver`. Clean layering: sans-IO core, then the tokio runtime, then
   the MoQ backends. Reverses the 2026-10-06 crate decision for the async
   layer only; quinn-udp stays a moq-sock module.
2. **A `tokio` feature on moq-quic.** No new crate. But the gate would cover
   the driver's logic, not just a dependency, so its tests leave the default
   merge gate, against `rs/AGENTS.md`.
3. **Keep it in moq-tokio, with moq-net behind a default feature.** The
   smallest move, but every moq-net use in moq-tokio needs a gate, and a
   driver-only build keeps the `quic` feature's other dependencies, such as
   reqwest and notify.

In every option:

- First, a separate PR replaces `Wire` with counters owned by the driver.
  moq-tokio converts at its boundary, and moq-net's wire stats do not change.
- `quic::web` and `quic::backend` stay in moq-tokio, and
  [Drop web-transport-trait](/quest/m2/drop-web-transport-trait.md) proceeds
  as planned on top. The driver stays public.
- moq-uring's driver is out of scope. It is `!Send`, and coupled to moq-net
  through `server::Activity` as well as `Wire`.
- Land after the receive-lease PR (#5878), which rewrites the driver's receive
  path.
- Publish the driver crate, if option 1, with moq-quic, moq-sock and moq-clock
  in the same release.

To settle with the maintainer: the option, the crate name, and whether the
decision belongs in m1, since it shapes what the cut publishes, while the move
itself stays m2.
