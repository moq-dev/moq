# [S] Bindings address groups by start

## Goal

moq-ffi, `moq-c`, and every language binding expose groups by start
timestamp instead of sequence number, including `seek` for GOP-aligned
failover, with each `doc/lib` page and the PR's Impact section saying how to migrate.

## Plan

Follow the cross-package sync table from `rs/moq-ffi` outward. Timestamps
already cross the bindings as `{value, scale}`, so a start reuses that
type. Decided 2026-11-03 (maintainer, replacing 2026-10-27's "producer arguments
keep their optional timestamp"): every read and write carries a timestamp.
Reads drop the `Option` that exact timestamps keeps for untimed, producer
arguments take a required one, and nothing is wall-clock stamped. A
scale-less track's frames carry their group's key. See
[the model](/quest/m2/time-addressing/model.md).

Resized from M to S 2026-11-14 (the m2 triage before the cut, under
/quest-afk, for the maintainer's review): [moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638)
already makes every binding's timestamps required and fills each group field
with the start, so what remains is the names. moq-ffi still says `sequence`
where it now means the start: the group record's field, `fetch_group`'s
argument, `seek`, and the docs on the ordered and arrival readers
(`rs/moq-ffi/src/consumer.rs`, `media.rs`). Rename them to `start` the same
way in moq-ffi, `moq-c`, and every binding, mirroring Rust's `group::Info`.
It stays in m2 because the rename is breaking and the cut would otherwise
publish a `sequence` that holds a start.

Public API: breaking in every binding. Wire: none.

## Required

- [Rust groups by start](/quest/m2/time-addressing/model.md) - the API these wrap
