# [S] A side declares which Setup extensions it offers

## Goal

A moq-net client or server, moq-tokio's dial and listen `Config`, and js/net's
`connect` and `accept` declare which Setup extensions they offer with one
positive `#[non_exhaustive] setup::Extensions { auth, solicit }`, all on by
default, so an application can decline the AUTH or solicit extension. Later
extensions join the same struct.

## Plan

Decided 2026-10-01 (Q2 on [#4675](https://github.com/moq-dev/moq/pull/4675)),
which built it. [#5106](https://github.com/moq-dev/moq/pull/5106) carves it
out so it lands before
[Request tokens](/quest/m1/auth/request-token.md).

- `setup::Extensions` in `rs/moq-net/src/setup.rs`, set through
  `Client::with_extensions` and `Server::with_extensions`. It folds in the
  private bools `run_setup` takes in `rs/moq-net/src/ietf/session.rs`.
- moq-tokio: an `extensions` field on `connect::Config` and `listen::Config`,
  serde, omitted when default.
- js/net: `extensions` on `ConnectProps` and `AcceptProps`.
- A side serves an inbound AUTH only when it offered AUTH itself.
- Wire: a draft-17+ server now omits AUTH from its SETUP when the client did
  not offer it, where it always sent it before. List this in the PR.
- Tests, Rust and JS: declining each extension keeps it out of SETUP, and an
  inbound AUTH on a side that declined it is refused.

Public API: additive, `setup::Extensions`, `Client::with_extensions`,
`Server::with_extensions`, the moq-tokio `extensions` fields, and JS
`Extensions`.
