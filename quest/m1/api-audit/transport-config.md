# [M] Transport config

## Goal

moq-tokio and moq-quic configuration has one meaning for an unset value,
types modes as enums, has no fields that only one other field reads, and
ships no leftover features.

## Plan

Decided 2026-10-22 (API audit):

- Optional durations: `None` means default in `connect`, `listen`, and
  `quic`, yet hand-written `Default` impls fill `Some(DEFAULT_*)`;
  `websocket::Options.timeout = None` means forever while `listen.timeout`
  uses `Some(ZERO)` for forever. Derive `Default` as all-`None`, keep
  defaults in `resolve()`/`Resolved`, and make zero always mean off.
- The `__cli_*` shadow `Duration` fields (about 10 in moq-tokio, plus relay
  `config.rs`, `acme.rs`, `stats.rs`, `cache.rs`) let a CLI value silently
  override an embedder's assignment; use one humantime newtype field per
  duration.
- `quic.keylog: Option<bool>` plus `SSLKEYLOGFILE` (failing `KeylogUnset`)
  becomes `keylog: Option<PathBuf>` like `qlog`.
- `server::Config.tls` and `listen.tls` are exclusive by doc comment only,
  and `tls::Listen.resolver` errors with either; `server.tls` overrides
  `listen.tls`, and the resolver becomes `tls::Serve::resolver(r)`.
- `Request::ok()` returns `(Session, JoinHandle)` and most callers discard
  the handle; return `Session` and surface a driver panic through
  `closed()`.
- Delete `quic::Connection` (only wraps `set_limits`),
  `worker::Config::with_pin` (unused), and one of `connect.once` or
  `Client::with_reconnect(bool)`; the QUIC load balancer gets one config
  path (`[listen.lb]`).
- moq-quic's unused features, `rustls` alias, and crate docs moved to
  [quic-api trim](/quest/m1/quic-api/trim.md) (2026-10-23).
- Public only for tests: `lease::Producer::{due, failed}` and `Due` go
  `pub(crate)` (drop the `tokio` feature), `connect::Config.quic` and
  `listen::Config.quic` (read only by `deprecated()`) go `pub(crate)`, and
  moq-uring `manual` and kio `sim` are renamed `_manual`/`_sim` or gated.

Public API: moq-tokio breaking. Wire: none.
