# [S] A JSON stream refuses an oversized record without ending

## Goal

A `@moq/json` or `moq-json` Stream write that would exceed the group budget
(32 MiB or 8192 frames) throws `GroupTooLarge` and leaves the log intact. A JS
reader that subscribes to a closed local track gets the close error at once,
as Rust's does, instead of waiting forever.

## Plan

Today `js/net/src/group.ts` wipes and closes the group on overflow and
`js/json/src/stream/producer.ts` aborts the track, which ends the log for
every reader. That ending is deliberate for a failed write
(`rs/moq-json/src/stream/mod.rs`), but a refused record is not a hole, so it
need not end anything.

Decided (2026-10-04):

- The Stream producer checks the remaining bytes and frames before writing and
  throws without touching the group, in JS and Rust. A failure past that
  check still aborts the track.
- `js/net/src/broadcast.ts` drops a closed local producer and queues a request
  nothing serves. A subscribe there surfaces the close error, matching the
  Rust test `a_failed_write_aborts_the_track`.
- Document the budget in `doc/lib/js/json.md` and the Rust crate docs.

Tests: an oversized record throws and the next record is readable; a late
JS subscriber after an abort errors.

## Closes

- [#4771](https://github.com/moq-dev/moq/issues/4771) - close this issue when the quest finishes
