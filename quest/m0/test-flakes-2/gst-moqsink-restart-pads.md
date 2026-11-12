# [S] moq-gst's moqsink restart pad test holds up

## Goal

moq-gst's
`source::imp::session_tests::a_moqsink_restart_keeps_pads_when_caps_arrive_at_different_times`
passes reliably, fixed at its cause in moqsrc (or the test, if the test is
wrong), and its `#[ignore]` is removed.

## Plan

It failed three times in CI, always on the generation 2 assertion
`playing.iter().all(|pad| held.contains(pad) && pad.starts_with("video_"))`,
about 1 s into the test:

- 2026-10-16, `Check` on [moq-dev/moq#5715](https://github.com/moq-dev/moq/pull/5715)
  ([run 38971716370](https://github.com/moq-dev/moq/actions/runs/38971716370)).
- 2026-11-07 05:07Z, `Check` on [moq-dev/moq#8122](https://github.com/moq-dev/moq/pull/8122)
  ([run 42341390803](https://github.com/moq-dev/moq/actions/runs/42341390803)).
- 2026-11-12, the merge queue for [moq-dev/moq#8551](https://github.com/moq-dev/moq/pull/8551)
  ([run 43250788147](https://github.com/moq-dev/moq/actions/runs/43250788147)), on
  main at 8c8f0effaa. #8551 only changes moq-net's group error caching, and
  the previous queue run passed this test.

The first two predate [moq-dev/moq#8160](https://github.com/moq-dev/moq/pull/8160)
(a failed track is not replayed on its pad) and
[moq-dev/moq#8526](https://github.com/moq-dev/moq/pull/8526) (a track's end is
recorded even when its instance closed first). The third came after both, so
some replay path is still open, or the earlier failures had another cause.
None of the 1,181 other failed-run logs from 2026-10-12 to 2026-11-12 that
were scanned show this test failing.

Not reproduced locally on 8c8f0effaa, with or without #8551 merged, on a
24-core host: 200 serial runs; 400 runs at 48 in parallel under
`quest gate`; 360 runs of the whole `session_tests` module at 12 and at 48
copies in parallel; and 200 runs at 4 copies pinned to one core.

What the assertion means: each pad's probe counts stream-starts after
`pad-added`. moqsrc's `begin` pushes the first stream-start before `add_pad`,
so a pad's count is the generation it plays. In generation 2 only video is
chained, and audio must end with EOS. The loop ends once one pad shows
count-2 buffers and audio's EOS arrives. A pad added late would need two
further stream-starts to reach count 2, so the likely failure is the audio pad
showing count-2 buffers. That means a
third `begin` on the audio pad: a second play of generation 1's audio, whose
replayed group reads as generation 2. With a single serial run of the test,
each of generations 1 and 2 reconciles two catalog snapshots, which differ
only in the media clock and `join`, so neither reshapes a rendition. That
leaves these ways for a reader to play its rendition again on one instance:

- `Follower::exited` on `Exit::Unlisted` while the rendition is listed,
  which calls `play()`.
- `Follower::start` with the same instance announced again, which plays
  every idle rendition.
- A reader that exited `Done` without a `Finished` record (subscription
  failed while the instance was up, a push error, an invalid buffer), then a
  later non-final catalog snapshot's `play()`.

Steps:

- The test's assertion now prints `held`, `playing`, and every `(pad, stream,
  eos)` the generation saw. Reproduce with that, or force each candidate path
  above by delaying it, such as holding the generation 1 catalog's second
  snapshot until audio's track has ended or failed.
- Fix the path at its cause in moqsrc, so a rendition never replays a group
  on its held pad, then remove the `#[ignore]` and the
  `sh/gh/test-inventory.allow` entry.

Public API: none expected. Wire: none.
