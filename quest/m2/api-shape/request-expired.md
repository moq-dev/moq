# [S] moq-tokio's Request takes no future

## Goal

`moq_tokio::server::Request` takes no future: `within(work)` is replaced by
`poll_expired`/`expired()`, which resolves once the accept deadline passes,
having refused the session with the timeout code, mirroring moq-net's
`Handshake::poll_expired` (`rs/moq-net/src/server.rs:1218`). moq-cli
(`main.rs:180`) and moq-relay race their admission against it, and
moq-relay's `incoming::Sealed` (`incoming.rs:116`) swaps `within` for
`expired`, so the tokio and io_uring paths (`incoming.rs:259-268`,
`uring.rs:827`) share one race in `connection.rs`.
The same PR rewrites every other `within` reference: the
`rs/moq-tokio/CHANGELOG.md` Unreleased entry, the `rs/moq-relay/MIGRATE.md`
Unreleased note, and the intra-doc link in `rs/moq-relay/src/auth.rs`
(`Auth::admit` docs), which otherwise fails `cargo doc -D warnings`.
Public API: `Request::within` removed, `Request::poll_expired`/`expired`
added; neither is on `release`. Wire: none.

## Plan

Decided 2026-11-11 (maintainer, rust2 /quest-unblock): callbacks and future
parameters are avoided where a closure-free API exists; the other seven
future- or closure-taking kio and moq-tokio helpers have none (`timeout`
already has `sleep_until`, `drive` has `time::run`, `wait` has `Pending`),
so they stay.
