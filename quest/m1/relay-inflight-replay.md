# [S] A Rust relay never replays from group 0 to a lite-03/04 subscriber

## Goal

A Rust relay or publisher serving a lite-03 or lite-04 SUBSCRIBE from a track
copy that is still resolving (an in-flight upstream request) starts at the
newest group once the copy resolves, as it does for lite-05+ and as JS does
for every version. It never replays the cache from group 0.

## Plan

Decided 2026-11-03 (maintainer, /quest-complete): ✅ land after
[HLS export on a declared grid](/quest/m0/hls-media-grid.md). The rule
(a pending join floors at request time) stays as implemented in
[moq-dev/moq#7737](https://github.com/moq-dev/moq/pull/7737). The one failing
test, `a_cursor_records_a_publisher_without_fetch`, fails because
moq-mux's `hls::Lend` subscribes to `video0` to build the `video0.hls` record
track before the recorder's media SUBSCRIBE exists, so groups 0 to 2 predate
that request. The HLS grid quest deletes the `<track>.hls` record tracks and has
the recorder read media tracks directly, which removes that early subscription.
Not chosen: the recorder naming a floor from its records (deleted with the
records); applying the rule only to upstream copies (still replays a local
cache to a first pre-05 subscriber); relaxing the test. Once the HLS grid lands,
merge main and confirm the recorder test passes unchanged.

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

Decided 2026-11-03 (maintainer, /quest-unblock): ✅ **A pending join floors at
request time.** A lite-03/04 SUBSCRIBE on a still-resolving copy floors at the
newest group as of the SUBSCRIBE's arrival, which is the upstream's answer, or
the first group when none existed then. It does not use the local cache's newest
group when the copy later resolves. A group produced after the request,
including in response to its demand, is delivered, and cached history before
the request is skipped. This is a relay rule that covers HLS recording and every
other late-resolving consumer.

- Not chosen: the HLS recorder requesting its own floor; waiting on
  [moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652).
- `rs/moq-hls/tests/integration/server.rs`
  `a_cursor_records_a_publisher_without_fetch` must pass unchanged, with no
  weakened assertion or timing wait. The controlled held-answer test (cached
  groups 0, 3 and 7 join at 7) keeps passing. Apply the same rule to lite-05+
  (`SubscribeServe`) and JS where they share the gap.
- Rerun `just ci`. Check whether the old-version finite-finish and CLI FETCH
  interop failures reproduce on base main, and record them as pre-existing if so.

Implementation: [moq-dev/moq#7737](https://github.com/moq-dev/moq/pull/7737)
implements this rule in Rust (`SubscribeServe` for lite-03 through lite-05,
the track model, and a front's splice) and JS. The rule holds in every
`moq-net` and JS test, but the HLS recorder test still fails until the HLS grid lands; see the decision above.

## Required

- [HLS export on a declared grid](/quest/m0/hls-media-grid.md) - removes the record-track subscription that captures media before the recorder's own SUBSCRIBE
