# [M] Restore the replacement media case: audio after a route switch

## Goal

`just test media --media-cases replacement` passes every run on Chromium under CPU load, and the
default media run runs it again. Remove its quarantine in `test/interop/clients/js/media.ts` and
`test/interop/README.md`.

## Plan

Evidence from the quest's PR, with the case at `ce616b52c` and `cbb742cec` on a loaded host
(`quest gate just test media --media-cases replacement` in a loop, with and without 24 busy
loops): it failed 1 of 8, then 2 of 12 runs, and none of 31 runs after a temporary `console.warn`
per decoded audio frame, so it is a timing race. Each failure is `audioStalled:true` in
`live incumbent fallback plays` (lite-06, lite-07) or `live replacement plays`, with the picture
painting.

One failing lite-07 fallback, timed from the log:

- 238 ms after the newer publisher stopped the incumbent's announce returned; its audio
  subscription was ok at 298 ms. The incumbent's `fetch error: ... group=62 error=group is past
  the budget` and `skipping missing groups: track=audio 0 -> 69` follow, so the first audio group
  the reader got was 69, about a second of retained audio behind the incumbent's live picture
  (the stalled ring's `audioTimestamp` 9507 ms against a picture at 10548 ms).
- 600 ms after that first group, at the 1039 ms bound, the ring was still stalled.

Open: where those 600 ms go. The decoder resets the ring on a changed instance
(`js/watch/src/audio/decoder.ts` `#runDecoder`), so the join snaps onto the clock; the question is
whether the ring waits for audio at the clock behind a backlog it must drop (then a join at the
live edge is the fix), or the starved page just decodes late (then the bound needs a load term
derived from the host, not a constant). Trace the ring's join and un-stall in the worklet on a
failing run; the largest-group replay at tune-in
([moq-dev/moq#8844](https://github.com/moq-dev/moq/pull/8844)) may overlap.

Public API: none. Wire: none.
