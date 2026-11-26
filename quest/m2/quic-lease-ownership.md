# [S] Guard Lease with stream and connection ownership and remove Default

## Goal

`moq_quic::stream::Lease` no longer implements `Default` and carries its `(StreamId, ConnectionTag)` ownership so handing an empty or wrong-stream `Lease` to `release()` or `restore()` panics cleanly on mismatch instead of underflowing `Leased::held`, and dropping a `Lease` cannot silently strand `lease.out = true` and stall connection-wide eviction. `transport::Config::validate()` also rejects `mtu.minimum_change == 0`.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ remove `Default` on `Lease` and tag it with `(StreamId, ConnectionTag)` before 0.22 in `m2` (rejected: keeping `Default` and guarding empty/mismatched leases at runtime). `moq-quic` publishes to crates.io for the first time at 0.22, so fixing this public API shape now avoids a breaking release right after the cut.

Caused by: [moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950) (`rs/moq-quic/src/connection/streams/lease.rs:218-228, 497-516`) and `mtu::Config::minimum_change` left unchecked in `rs/moq-quic/src/config/transport/mod.rs:204-221`.

Guidance:

- Remove `#[derive(Default)]` from `Lease` and tag each issued `Lease` with `(StreamId, ConnectionTag)` like `send::Group` and `send::Offset`.
- Assert ownership in `recv::Stream::release` and `recv::Stream::restore`, and make sure `moq-tokio` and `moq-uring` hold `Option<Lease>` rather than `Lease::default()`.
- Decide and test how a dropped `Lease` behaves (e.g., debug-assert or panic-safe cleanup on stream drop/stop) so `lease.out` cannot permanently block eviction connection-wide.
- Reject `mtu.minimum_change == 0` in `transport::Config::validate()` with `ConfigError::Mtu`, preventing the infinite probe loop and `upper_bound` underflow in `mtud.rs:319-329`.

Public API: breaking (`moq_quic::stream::Lease` drops `Default`; `transport::Config::validate` rejects `mtu.minimum_change == 0`). Wire: none.

## Related

- [Receive-window eviction deadlocks](/quest/m1/quic-eviction-deadlock.md) - fixes the `stuck_waiting` and `Chunks::finalize_inner` deadlocks in `lease.rs`
