# [S] Tests pass when they share one process

## Goal

The Rust tests pass under plain `cargo test`, where every test in a binary
runs as a thread of one process, as well as under nextest's
process-per-test. No test writes process-wide state: none calls
`std::env::set_var` or `remove_var`, and none unwraps an install of the
process-wide rustls `CryptoProvider`.

## Plan

Decided 2026-10-22 (maintainer): read `SSLKEYLOGFILE` in one place and pass
the resulting path down as an argument. Tests pass a path and never touch the
environment. Public behavior and API are unchanged. Rejected: a shared env
lock (concurrent `setenv` and `getenv` on other threads stays undefined
behavior on glibc), and a public keylog path in the QUIC config (nothing
consumes it).

Keylog tests that set or clear `SSLKEYLOGFILE` today, each commented
"nextest runs each test in its own process":

- `rs/moq-tokio/src/quic.rs` `keylog_requires_a_path` (the env read is in
  `validate`, around `:576`)
- `rs/moq-tokio/tests/integration/keylog.rs`: three tests share `handshake()`
- `rs/moq-ffi/src/test.rs` `client_keylog_writes_session_keys`
- `rs/moq-c/src/test.rs` `server_keylog_requires_sslkeylogfile`
- `rs/moq-relay/src/uring.rs` `bind_refuses_a_keylog_without_sslkeylogfile`
- `rs/moq-gst/src/source/imp.rs` `keylog_reaches_the_client`

Crypto provider: `rs/moq-relay/tests/integration/wire_stats.rs:20` unwraps
`install_default()` and `stack.rs:8` expects it. Every other module in that
binary ignores the result, so a second install panics. Match the rest, or
install once for the binary.

Grep the workspace for other `set_var`/`remove_var` calls in tests (moq-cli's
`EnvGuard` serializes its own) and fix any that leak. Verify with a
`cargo test` run of each touched crate as well as nextest. Make the
`set_var` ban stick if a lint can do it cheaply, for example clippy's
`disallowed_methods` in test code.

Public API: none. Wire: none.
