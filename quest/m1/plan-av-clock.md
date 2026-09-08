# [S] Plan: the audio playhead drives video sync while audio plays

## Goal

A written decision, recorded in this file and then executed as its own quest,
on how `js/watch` keeps audio and video in sync. Today the audio ring is
depth-driven and free-running on the AudioContext clock while video paces
against `Sync.reference`, a wall clock anchored at the earliest arrival.
Nothing ties them; A/V sync is emergent from both targeting the same delay,
so a ring that re-buffers or skips drifts against video until the next
re-anchor. The outcome is an audio-master clock: `Sync.reference` derives
from the audio playhead while audio plays and falls back to the wall clock
when muted or video-only.

## Plan

Branch from dev; the `Sync` inputs are public `@moq/watch` API and #3517
already reshaped them there.

Two API shapes were on the table on 2026-09-07 and neither was chosen:

- Per-track handles: `sync.track("audio")` / `sync.track("video")`, each
  reporting its own advertised delay and measured spread and one of them
  nominated as the clock source. Recommended then, because it removes the
  `audio`/`video`/`audioSpread`/`videoSpread` quartet of inputs and lets a
  third track (text) join without another pair.
- `delay` as a floor only: keep the flat inputs, drop `"auto"`, and let the
  audio playhead be the reference whenever an audio track is active.

Decide with the estimator in hand: what the audio decoder can expose as its
playhead across the isolated and postMessage ring paths, how the video
decoder's per-frame `sync.wait()` reads it without a cross-thread hop per
frame, and what happens at the transitions (mute, audio track end, the ring
re-stalling). Record the verdict here, re-title this file as the
implementation quest, and re-estimate it.

## Related

- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - the estimator this sits on
- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - stretching needs a clock to converge toward
