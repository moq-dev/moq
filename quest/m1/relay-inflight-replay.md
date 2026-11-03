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

- `cursor_floor` (`rs/moq-net/src/lite/publisher.rs:127` at be39188000)
  floors a pre-06 SUBSCRIBE at `latest`, the newest group when it arrived,
  and at 0 when the copy has none yet. [moq-dev/moq#7328](https://github.com/moq-dev/moq/pull/7328) (merged) re-reads the newest group of an unresolved track
  once it resolves, but only for lite-05+ (`SubscribeServe`, the
  `has_track_stream()` match at `:2259`). JS no longer replays for any
  version.
- Extend the re-read to lite-03/04, so a resolved copy floors them at its
  newest group, never at a lower first group it reports; match what JS
  does.
- Test with a lite-03 and a lite-04 subscriber on a relay whose upstream
  answers after the SUBSCRIBE, with cached groups below the live edge.
  [moq-dev/moq#7328](https://github.com/moq-dev/moq/pull/7328)'s interop was not rerun after its last changes; run the lite-03/04
  cells here.

Public API: none. Wire: none.

Implementation checkpoint: [moq-dev/moq#7737](https://github.com/moq-dev/moq/pull/7737)
removes the confirmation-time version guard and adds a controlled held-answer test.
The test exposes cached groups 0, 3, and 7 with source start 0, and checks the
encoded group streams. Lite03/04 join at 7; explicit starts and arrival floors
remain covered, with lite05/06 controls. The range-anchor integration expectation
now includes lite03/04 among the versions whose unfloored copy joins live.

## Waiting

- Maintainer: settle the HLS recorder startup contract before this behavior lands.
  The live-edge fix makes
  `moq-hls::integration server::a_cursor_records_a_publisher_without_fetch`
  time out with `moq-lite-03: no segment recorded`; the same test passes with
  `origin/main`'s publisher implementation. The normal test first reports this
  on lite01; a diagnostic run limited to lite03 confirms that restricting the
  fix to lite03/04 would not avoid it. `Demand::wanted` includes queries, and
  pre05 copies expose synthetic info before their upstream SUBSCRIBE answer,
  so neither signal establishes that a media subscription can retain the
  recorder's first segment. Options: (1) preserve the settled live-edge join
  and make HLS recording request an explicit media floor derived from its
  records (recommended), or (2) revisit how a pending join distinguishes cached
  history from media produced in response to its demand. Do not weaken the
  recorder assertion or add a timing wait. Complete the source fix, rerun
  `just ci`, and run all-language interop if this extends into the gateway.
  The twelve requested lite03/04 Rust/browser-JS publisher by Rust/Node/Bun
  subscriber matrix cells passed. The broader interop invocation also runs
  finite-finish and CLI FETCH cases that failed on these old versions; their
  applicability and base behavior still need verification before claiming
  that broader suite passes.
