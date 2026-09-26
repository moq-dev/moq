# [M] Paced replay

## Goal

A live player plays a replayed recording unchanged: `moq import archive`
publishes each recorded track to SUBSCRIBE as well as FETCH, pushing its groups
when a shared clock reaches their media time, like `ffmpeg -re`. `export ts`
and web watch play a replay the way they play a live broadcast, and a viewer
who joins late joins mid-replay.

## Plan

- Today `moq_archive::Reader` publishes only the timeline track live and serves
  media groups on FETCH through `track::Dynamic`, so a subscriber sees none.
- One clock per import, not per subscriber. It starts at the earliest recorded
  timestamp across the selected tracks and every track paces against it, so
  tracks stay in sync and every viewer sees the same moment.
- A timeline gap is skipped at pace (the clock keeps running). A growing
  archive is followed as new timeline segments land. A finite archive ends
  each track after its last group.
- FETCH keeps serving any advertised group, so DVR and HLS are unaffected.
- The API shape (a reader option vs a separate paced publisher) and the CLI
  default follow the root API rules; report both in the PR. Update
  `doc/bin/cli.md`.
- Test with paused time: record, replay paced, and assert a plain subscriber
  gets every group in order at media pace, that a late subscriber starts at the
  current group, and that two tracks stay aligned.
