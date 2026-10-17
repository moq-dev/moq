# [S] A container decode error ends the reader in Rust and JS

## Goal

A container payload that fails to decode ends the media reader with that
error, the same way in Rust and JS. Only a transport abort or eviction of a
group is skipped as a gap. Nothing logs a decode error and keeps reading.

## Plan

Rust already does this in the main read path (`moq-mux` container consumer
ends the reader on a payload decode error), with two holes:

- JS `@moq/hang`'s container consumer marks the group truncated on any
  error, keeps the frames it decoded, and lets the rest reach `spawn`, which
  only logs. The reader never fails, so malformed media reads as loss.
- Rust's `poll_min_timestamp` lookahead returns the error, but its callers
  (startup, the max-end scan, the rewind check) discard it. The bad frame is
  already consumed, so a malformed object at the head of a group then reads
  as a clean or short end.

Decided 2026-10-17 (maintainer): a quest of its own in m1, since both are
silent data loss. Found while reviewing #5815; the JS behavior was declined
there as out of scope.

Each fix lands with a regression test that feeds a malformed object (head of
group and mid-group) and asserts the reader's error, in both languages.

Public API: JS readers can now reject with a decode error where they used to
truncate. Wire: none.

## Related

- [Container reads drop backfill](/quest/m1/read-backfill.md) - the other Rust/JS container-consumer parity gap
- [Dropped sources](/quest/m1/dropped-sources.md) - consumers see the producer's real error on every end
