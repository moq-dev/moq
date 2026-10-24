# [S] Landing a group skips the parked lock when nothing waits

## Goal

`expiry::Wakes::landed` costs no lock when no reader is parked, cutting about
600 instructions from every group's arrival.

## Plan

`landed` (`rs/moq-net/src/model/expiry.rs`) takes the `parked` mutex on every
group, even when `landings` is empty. Add a fast path that skips it, with a
written memory-ordering argument that a reader parking concurrently is never
missed, and a loom model for that race. Measure on `track_stamp_groups` and
`session_delivery_paced`.

Decided 2026-10-23 (maintainer): only this fast path. The rest of
`session_delivery_*` stays accepted as recorded in
[the epic](/quest/m1/release-perf/README.md).

Public API: none. Wire: none.
