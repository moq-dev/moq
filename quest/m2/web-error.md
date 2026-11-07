# [S] One typed WebTransport error per QUIC adapter

## Goal

moq-tokio and moq-uring each expose one typed `quic::web::Error`, with
`Connection`, `Closed`, `SessionGone`, `UnknownSession`, `Http3`, `Handshake`,
`Read`, and `Write` variants, before the next cut. moq-uring's `Web` and
`Http3` variants move out of its `quic::Error`, and the `SessionError` alias is
dropped. Callers match on a variant instead of parsing a string. The break
lists its migration in each crate's `MIGRATE.md`.

## Plan

Decided 2026-11-06 (maintainer, API audit decision 3, from
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088)): one typed
`web::Error` per crate, as a follow-up before the cut rather than inside the
audit PR.

Today the errors are split across `quic/web/{recv,send}.rs` in both crates,
and moq-uring's `quic::Error` carries `Web(String)` (`rs/moq-uring/src/quic/mod.rs`).
