# [M] Bindings address groups by start

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
[the model](/quest/m1/time-addressing/model.md).

Public API: breaking in every binding. Wire: none.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the API these wrap
