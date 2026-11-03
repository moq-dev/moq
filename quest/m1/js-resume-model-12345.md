# [S] The JS resume model passes seed 12345

## Goal

`@moq/net`'s resume model passes
`MOQ_RESUME_SEED=12345 MOQ_RESUME_CASES=400 bun test src/resume.model.test.ts`
(in `js/net`), fixed at the cause: a reader whose range was fully read and
then updated to no ranges keeps reading instead of reporting the track ended
cleanly. The minimal case is a fixed regression test.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1
- m2

Found by [moq-dev/moq#7681](https://github.com/moq-dev/moq/pull/7681), which
reproduced it at its merge base `8121cf68`: "the track ended cleanly though no
route declared an end". A reader whose `[0,1)` range was read, then updated
to `ranges: undefined`, reports done.

- An update to no ranges means "follow live" again, not "every range is
  covered"; find where the reader's completion check reads the emptied range
  list as exhausted.
- Sibling of [moq-dev/moq#7705](https://github.com/moq-dev/moq/pull/7705) on
  the Rust side; compare the two fixes so the languages agree.

Public API: none. Wire: none.

## Related

- [The Rust resume model fix](https://github.com/moq-dev/moq/pull/7705) - the Rust model's own seed 12345 failure
