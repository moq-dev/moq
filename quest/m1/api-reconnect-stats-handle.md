# [XS] moq_tokio::ConnectionStatsReader gets a name that says what it is

## Goal

The cloneable handle `Connection::stats()` returns, which observes the live
connection across reconnects (`stats`, `snapshot`, and the `presence` wait
path #3679 adds), has a name that reads as a role rather than as "reader of
stats", and sits under its module like the other connection types.

## Plan

Rename `ConnectionStatsReader` (`rs/moq-tokio/src/connection.rs`,
re-exported from the crate root with `ConnectionSnapshot`). Consumers:
`rs/libmoq/src/session.rs` and `rs/moq-gst/src/sink/session.rs`.
Recommended: `moq_tokio::connection::Monitor`, obtained through
`Connection::monitor()`, with the `connection` module made public so the
short name sits under its namespace; `ConnectionSnapshot` becomes
`connection::Snapshot` in the same move and both leave the crate root.
Alternative: keep the root re-export and call it `ConnectionMonitor`. Ask
before landing either.

Public API: breaking on moq-tokio, so on dev. Wire: none.

## Related

- [Rate estimate names](/quest/m1/api-rate-estimate-names.md) - the estimates this handle exposes get one name everywhere
