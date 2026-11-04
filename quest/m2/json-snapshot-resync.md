# [S] A JSON snapshot consumer resyncs after a bad frame

## Goal

When a frame fails to decode (a `SyntaxError` or `DuplicateKey`), `@moq/json`'s
`Snapshot.Consumer` drops the current group and throws a distinct decode
error, then resumes at the next group's snapshot. Callers such as
`@moq/room` and moq-boy can catch that error and keep reading instead of
ending. `rs/moq-json`'s snapshot consumer does the same.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

From [moq-dev/moq#7693](https://github.com/moq-dev/moq/pull/7693)'s
follow-ups, where both automated reviewers asked moq-boy to keep reading
after a malformed frame. That was refuted there, since the consumer can't
recover: after a parse error `#framesRead` and the delta base go stale
(`js/json/src/snapshot/consumer.ts`; `frames_read` in
`rs/moq-json/src/snapshot/consumer.rs`), so continuing would show wrong
state. The maintainer approved planning the fix from #7693's iterate.

- On a decode failure, reset the frame count and delta base, skip the rest
  of the group, and throw a new typed error (wrapping the cause) so callers
  can tell it apart from a track end.
- Mirror it in Rust as a new `Error` variant, with the same skip.
- Move `@moq/room`'s metadata reader and moq-boy's status loop
  (`js/room/src/metadata.ts`, `js/moq-boy/src/game.ts`) to skip the error
  and resync, and test a malformed delta followed by a good group in both
  languages.

Public API: a new decode error in `@moq/json` and `moq-json`. Wire: none.

## Related

- [Benchmark a Value merge-patch diff for moq-json](/quest/m2/json-value-diff.md) - may replace the Rust diff this consumer applies
