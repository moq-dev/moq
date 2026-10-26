# [S] Rust keeps tokens through a drain

## Goal

A Rust `Connection` closed while a token-only publication drains keeps that
token's grant until the drain ends, as JS does since
[moq-dev/moq#6630](https://github.com/moq-dev/moq/pull/6630).

## Plan

- Suspected gap: `Connection::close` (`rs/moq-tokio/src/connection.rs`) drains
  the session, but `Shared::end` aborts the shared `auth::Handle` when the
  loop task ends. Write the test first; fix only if it fails, and delete this
  quest if it passes.
- Carrying tokens across reconnects already works (one handle shared across
  sessions).
