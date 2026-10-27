# [M] Bindings address groups by start

## Goal

moq-ffi, `moq-c`, and every language binding expose groups by start
timestamp instead of sequence number, including `seek` for GOP-aligned
failover, with each `doc/lib` page and the PR's Impact section saying how to migrate.

## Plan

Follow the cross-package sync table from `rs/moq-ffi` outward. Timestamps
already cross the bindings as `{value, scale}`
([exact timestamps](/quest/m1/binding-timestamps.md)), so a start reuses that
type. Decided 2026-10-27 (maintainer): every track is timed after the cut,
so reads drop the `Option` that exact timestamps keeps for untimed. Only
reads drop it here; producer arguments keep their optional timestamp.

Public API: breaking in every binding. Wire: none.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the API these wrap
- [Exact timestamps in every binding](/quest/m1/binding-timestamps.md) - the timestamp type a start uses
