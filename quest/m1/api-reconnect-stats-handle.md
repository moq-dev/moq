# [XS] moq_native::ConnectionStatsReader gets a name that says what it is

## Goal

The cloneable handle `Reconnect::stats()` returns, which observes the live
connection across reconnects (`stats`, `snapshot`, `presence` and its wait
path), has a name that reads as a role rather than as "reader of stats", and
matches how the other reconnect types are named.

## Plan

Rename `ConnectionStatsReader` (`rs/moq-native/src/reconnect.rs`, re-exported
from the crate root). Consumers: `rs/moq-gst/src/sink/session.rs` and
`rs/libmoq/src/session.rs`. Recommended: `moq_native::reconnect::Monitor`,
obtained through `Reconnect::monitor()`, with the `reconnect` module made
public so the short name sits under its namespace; `ConnectionSnapshot`
becomes `reconnect::Snapshot` in the same move. Alternative: keep the root
re-export and call it `ConnectionMonitor`. Ask before landing either.

Public API: breaking on moq-native, so on dev. Wire: none.

## Required

- PR #3679 has merged - the `presence` and wait surface this quest renames
  comes from it

## Related

- [Rate estimate names](/quest/m1/api-rate-estimate-names.md) - the estimates
  this handle exposes get one name everywhere
