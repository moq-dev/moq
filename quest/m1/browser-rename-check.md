# [S] A browser follows a renamed track

## Goal

A real browser keeps playing when a codec identity change makes
`@moq/publish` mint a new `track` for the same rendition, and when coded-size
growth keeps the codec and therefore the existing track. This follows the
merged #5552 contract: coded size alone is mutable. The check runs at least
nightly in CI.

## Plan

#5552 landed with unit tests only and listed this check as a follow-up. Unit
tests do not show what WebCodecs and the watch path do across the switch.

Extend the browser media QA (`just test media`) rather than building a new
harness. Fail on a stall, a decode error, or a frame from the old track after
the switch. Check that it fails with the rename disabled.

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
key that is not a keyframe after reconfiguration. The exact production source
was restored before committing; no product behavior changes in this draft.

The whole oracle is not green. The original full run stopped before the new
case at cold-start audio tone (59/70 samples, below the unchanged 90% floor).
The focused run subsequently failed post-resize audio tone (67/75 samples),
with silent/stalled intervals. These observations do not establish their cause
or a connection to the isolated Opus scheduling finding. Keep this quest until
smooth playback is established with the complete applicable oracle; no timing,
tone or skew threshold has been relaxed.

## Related

- [Rust identity enforcement](/quest/m1/catalog-identity-rs.md) - the same contract for the Rust publishers

- [Capture channel count and cold-start tone](/quest/m1/publish-audio-channel-count.md) - already owns the unpinned cold-start audio-tone symptom
- [Isolated Opus target](/quest/m1/audio-jitter-target/opus-isolated-target.md) - related playout stalls, with no shared cause established
