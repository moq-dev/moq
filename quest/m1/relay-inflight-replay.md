# [S] A Rust relay never replays from group 0 to a lite-03/04 subscriber

## Goal

A Rust relay or publisher serving a lite-03 or lite-04 SUBSCRIBE from a track
copy that is still resolving (an in-flight upstream request) starts at the
newest group once the copy resolves, as it does for lite-05+ and as JS does
for every version. It never replays the cache from group 0.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7328](https://github.com/moq-dev/moq/pull/7328).

Milestone: ✅ m1 (moved from m2). Not chosen: m2. There is no m1
pipeline-requests epic, so this sits at the m1 top level.

- `cursor_floor` (`rs/moq-net/src/lite/publisher.rs:125`) floors a pre-06
  SUBSCRIBE at `latest`, the newest group when it arrived, and at 0 when the
  copy has none yet. [moq-dev/moq#7328](https://github.com/moq-dev/moq/pull/7328) (merged) re-reads the newest group of an unresolved track
  once it resolves, but only for lite-05+ (`SubscribeServe`, `:1993`). JS no
  longer replays for any version.
- Extend the re-read to lite-03/04, so a resolved copy floors them at its
  newest group, never at a lower first group it reports; match what JS
  does.
- Test with a lite-03 and a lite-04 subscriber on a relay whose upstream
  answers after the SUBSCRIBE, with cached groups below the live edge.
  [moq-dev/moq#7328](https://github.com/moq-dev/moq/pull/7328)'s interop was not rerun after its last changes; run the lite-03/04
  cells here.

Public API: none. Wire: none.
