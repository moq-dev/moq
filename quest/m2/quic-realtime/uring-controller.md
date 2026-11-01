# [S] io_uring workers run an application's own congestion controller

## Goal

moq-uring's QUIC transport config (`rs/moq-uring/src/quic/transport.rs`) takes a `moq_congestion::ControllerFactory` as well as its built-in `Congestion` families, and the relay's io_uring runtime installs the controller a `moq_tokio::quic::Config::controller` carries, instead of refusing that config. An application embedding the relay gets the same controller on either runtime.

## Plan

Follows [moq-dev/moq#7467](https://github.com/moq-dev/moq/pull/7467) (open), which adds `quic::Config::controller` to moq-tokio, a factory the QUIC backend builds one controller from per connection, and has the relay's io_uring runtime refuse a config that carries one. This waits on #7467 landing.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, in this epic, because shipping `Realtime` "in moq-tokio and moq-uring" (the README's own work) is simpler once moq-uring takes a factory like moq-quic's `transport::Config::congestion` does: the winner becomes one factory both runtimes install, not a new enum variant per crate.

- moq-uring's `Congestion` enum keeps its built-in families; add a factory variant or field, mirroring how #7467 shaped moq-tokio's, so the two crates' configs stay mirrored (the [moq-quic API](/quest/m1/quic-api/README.md) epic's rule).
- `rs/moq-relay/src/uring.rs` (the `transport.congestion` mapping, about line 500) passes the factory through and drops #7467's refusal.
- The io_uring workers are thread-per-core: the factory is shared across workers, and each connection's controller lives on its worker. State the factory's `Send + Sync` requirement in its docs.
- Test as #7467's `a_custom_controller_reaches_the_live_connection` does, on a ring worker: a counting factory builds the controller a live connection runs. Linux-only; run it in the `io-uring` nextest group.

Public API: a factory option on moq-uring's QUIC transport config; the relay's io_uring runtime accepts `quic::Config::controller`. Wire: none.
