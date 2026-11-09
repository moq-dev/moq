# [L] Connection state carries each phase's data

## Goal

A connection state snapshot carries each phase's data inside that phase, so
a field that means nothing in the current phase can't be read:

- moq-ffi's `MoqConnectionState` (`rs/moq-ffi/src/session.rs`) is a flat
  record (`revision`, `phase`, `connects`, `failure`, `version`), so
  `failure` and `version` are readable in phases where they mean nothing.
  It becomes a phase enum whose variants carry their own data, mirroring
  Rust's `moq_tokio::connection::Observation`, which takes the same shape,
  and every hand-written wrapper follows.
- moq-c's `moq_connection_state` follows the same shape and renames its
  `protocol` to `version`.

## Plan

Split from the FFI handle state quest (2026-11-08): it breaks every binding,
so under the [explicit-state epic](/quest/m1/explicit-state/README.md)'s
milestone rule it is its own m2 quest. Its PR marks its title `!` and says
how to migrate.

Rust's `moq_tokio::connection::Observation` (`rs/moq-tokio/src/connection.rs`)
is flat too: `failure`, `version`, `transport`, and `last_connected` sit
beside `phase`, while the private `Lifecycle` enum it is built from already
carries them per phase.

- ✅ Decided 2026-11-08 (maintainer): reshape `Observation` together with
  `MoqConnectionState`, one phase per value in both, so each binding converts
  one enum to another (rejected: reshape only the binding records and keep
  Rust's snapshot flat).

## Related

- [Lifecycles are explicit state](/quest/m1/explicit-state/README.md) - the epic this item split from
