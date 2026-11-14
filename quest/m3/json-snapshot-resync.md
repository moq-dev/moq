# [S] A JSON snapshot consumer resyncs after a bad frame

## Goal

When a frame fails to decode (a `SyntaxError` or `DuplicateKey`), `@moq/json`'s
`Snapshot.Consumer` drops the current group and resumes at the next group's
snapshot by itself, so callers such as `@moq/room` and moq-boy keep reading
instead of ending. `rs/moq-json`'s snapshot consumer does the same.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs).

From [moq-dev/moq#7693](https://github.com/moq-dev/moq/pull/7693)'s
follow-ups, where both automated reviewers asked moq-boy to keep reading
after a malformed frame. That was refuted there, since the consumer can't
recover: after a parse error `#framesRead` and the delta base go stale
(`js/json/src/snapshot/consumer.ts`; `frames_read` in
`rs/moq-json/src/snapshot/consumer.rs`), so continuing would show wrong
state. The maintainer approved planning the fix from #7693's iterate.

- Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): the consumer resyncs itself, with no new error variant. On a
  decode failure it resets its frame count and delta base, skips the rest
  of the group, and counts the skip (optionally a `skipped()` counter). It
  also logs a warning per skip, so a stream whose every group is malformed
  is visible to an operator rather than silently stale. A
  terminal error stays terminal, so `@moq/room` and moq-boy need no change.
- Do the same in Rust's snapshot consumer, and test a malformed delta
  followed by a good group in both languages.
- Added 2026-11-08 (maintainer, /quest-plan of an external code audit of
  `9f51fa0966`, which scored this 5/10): a malformed *snapshot* is worse than
  a malformed delta in both languages. The frame count stays at 0
  (`js/json/src/snapshot/consumer.ts:164-171`,
  `rs/moq-json/src/snapshot/consumer.rs:162-168`), so the next delta is
  applied as a snapshot and drops every field it doesn't repeat. Test that
  case too. Also, JS `Decoder.delta` and `snapshot`
  (`js/json/src/snapshot/decoder.ts:33`, `:43`) keep `#current` after a
  failure. Clear it, as Rust does (`decoder.rs:58`, `:88-90`), so a later
  delta fails instead of applying to stale state.

Public API: at most a `skipped()` counter in `@moq/json` and `moq-json`. Wire: none.
