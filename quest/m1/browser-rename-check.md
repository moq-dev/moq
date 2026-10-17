# [S] A browser follows a renamed track

## Goal

A real browser keeps playing when a codec identity change makes
`@moq/publish` mint a new track for the same rendition. A size-only change,
including coded-size ceiling growth, keeps the track identity and also plays
without a stall. The check runs at least nightly in CI.

## Plan

The publisher's `broadcast.ts` excludes size from decoder identity; its unit
tests cover same-track size changes. Exercise that contract through WebCodecs
and watch using the existing browser media QA (`just test media`).

Test a codec change that requires a new track and a size-only change that
must preserve it. Assert the expected identity in each case, continued
playback, and no decode errors. The codec case must fail with renaming
disabled; the size case must catch an unnecessary rename.

Public API: none. Wire: none.

### Evidence

2026-10-15: the existing media harness now exercises AVC to VP8, followed by
VP8 growing from 320x240 to 640x480. Presented pixels carry a source generation,
and readback checks actual decoded dimensions as well as the canvas. A focused
case keeps the content and lifecycle oracles while leaving the full run's
cold-start grading and unrelated controls explicitly unselected.

The real-browser focused run minted `video/1`, presented the codec transition
at 30.0 fps with median/p95 skew 0/0 ms, and played afterwards at 29.9 fps with
0/200 ms skew. The resize kept `video/1`, reached 640x480 decoded and presented
frames, and measured 29.9 fps with 0/200 ms skew. No previous-generation picture
returned after the new generation started presenting.

With the publisher's identity-minting block removed, the real-browser case
fails at the existing decode-error oracle: WebCodecs refuses a chunk marked as
key that is not a keyframe after reconfiguration.

The whole oracle is not green. The original full run stopped before the new
case at cold-start audio tone (59/70 samples, below the unchanged 90% floor).
The focused run subsequently failed post-resize audio tone (67/75 samples),
with silent/stalled intervals. These observations do not establish their cause
or a connection to the isolated Opus scheduling finding. Keep this quest until
smooth playback is established with the complete applicable oracle; no timing,
tone or skew threshold has been relaxed.

2026-10-16: the post-rename tone failure (CI: 71/81) was the replacement
track's jitter estimate restarting from its cold-start prior. The shared delay
dropped at promotion and climbed back, so the audio ring skipped and then
parked. A replacement consumer now continues its predecessor's estimate. Five
focused runs on a loaded machine (load 47 on 24 cores): three passed every
window; one failed coded-size growth tone (60/81) while the delay rose with
real video lateness on the same `video/1`; one failed on a subscribe update
racing the old track's finish (`stream aborted by the remote server`).

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - the same contract for the Rust publishers
- [Capture channel count and cold-start tone](/quest/m1/publish-audio-channel-count.md) - already owns the unpinned cold-start audio-tone symptom
- [Isolated Opus target (#5807)](https://github.com/moq-dev/moq/pull/5807) - related playout stalls, with no shared cause established
