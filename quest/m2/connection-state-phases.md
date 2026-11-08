# [L] Connection state carries each phase's data

## Goal

A connection state snapshot carries each phase's data inside that phase, so
a field that means nothing in the current phase can't be read:

- moq-ffi's `MoqConnectionState` (`rs/moq-ffi/src/session.rs`) is a flat
  record (`revision`, `phase`, `connects`, `failure`, `version`), so
  `failure` and `version` are readable in phases where they mean nothing.
  It becomes a phase enum whose variants carry their own data, mirroring
  whatever shape Rust's connection state takes, and every hand-written
  wrapper follows.
- moq-c's `moq_connection_state` follows the same shape and renames its
  `protocol` to `version`.

## Plan

Split from [FFI handle state](/quest/m1/explicit-state/ffi-state.md) when it
started (2026-11-08), as that item recorded: it breaks every binding, so
under the [explicit-state epic](/quest/m1/explicit-state/README.md)'s
milestone rule it is its own m2 quest. Its PR marks its title `!` and says
how to migrate.

Rust's `moq_tokio::connection::Observation` (`rs/moq-tokio/src/connection.rs`)
is flat too: `failure`, `version`, `transport`, and `last_connected` sit
beside `phase`, while the private `Lifecycle` enum it is built from already
carries them per phase. Open, for whoever starts this, with the maintainer:
reshape `Observation` the same way so the bindings mirror it (recommended,
since the bindings can then convert one enum to another), or reshape only
the binding records and keep Rust's snapshot flat.

## Related

- [Lifecycles are explicit state](/quest/m1/explicit-state/README.md) - the epic this item split from
- [Bindings' small surface fixes](/quest/m2/api-shape/bindings.md) - other breaking binding fixes before the cut
