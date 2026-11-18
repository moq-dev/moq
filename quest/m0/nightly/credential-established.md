# [S] Binding tests drop a session once the client saw it accepted

## Goal

The tests [moq-dev/moq#8817](https://github.com/moq-dev/moq/pull/8817) gave
real 1 s waits (Go `go/wrapper/reconnect_test.go`, Python
`py/moq-rs/tests/test_server.py`, `rs/moq-ffi/src/test.rs`, `rs/libmoq/src/test.rs`),
and the OBS tests in draft
[moq-dev/moq#9016](https://github.com/moq-dev/moq/pull/9016), drop the session
as soon as the client has seen the peer accept its credential, instead of
waiting out `Backoff::healthy`. No wire change.

## Plan

[moq-dev/moq#9038](https://github.com/moq-dev/moq/pull/9038) made the
reconnect policy (Rust `Reconnect`, JS `Reload`, every binding through
`moq-ffi`) count a session as established once the peer accepts the
connection credential: its SETUP, or on lite-07 the AUTH_OK to the
connection's own credential. moq-lite 03/04 keep the 1 s uptime bar.

What is left: a test can drop a session the moment the *server* accepted it,
but the client counts it only once it has *read* that answer, and a server
close can overtake it. No binding reports that moment: `Connected` fires when
the session goes live, before the answer. So the waits stay until the client
can observe acceptance.

## Waiting

- Maintainer: how should a binding observe that the peer accepted the session?
  (a) Recommended: moq-tokio's `Connection` reports Connected, and
  `established()` resolves, only once the peer accepted the credential (or
  the session passed `healthy` on moq-lite 03/04), and every binding's
  `Connected` phase follows. A refused session then never reads as
  connected, and `established().await` returns the refusal instead of `Ok`.
  It's a behavior change in every binding, but no API change. (b) Add an
  `accepted` flag to the state snapshot in every binding, which is new API that only
  tests need today. (c) Keep the 1 s waits and delete this quest. Check: the
  answer is recorded in this Plan.

## Related

- [Connect policy state](/quest/m4/connect-policy-state.md) - moves the same policy into one place per language
