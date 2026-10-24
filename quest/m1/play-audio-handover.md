# [S] moq play keeps its audio output open across a replacement

## Goal

When a broadcast is replaced or falls back, `moq play` switches to the new
run without closing and reopening its audio output. The switch then costs
only the configured routing, codec, and playback delays.
`just test native-play` passes repeatedly within its derived 1495 ms bound.

## Plan

The player now keeps one lazily opened speaker across broadcast runs. Existing
paused-clock replacement and offline-return tests verify one speaker opening,
and all 240 player unit tests pass. Each playback sink already resamples and
remixes its input, so a different input rate or layout does not require reopening
the device.

Finish the native validation before landing. A real-device run passed lite-07
replacement/fallback at 822/793 ms, but lite-06 fallback produced distorted PCM
past the unchanged 1495 ms bound. Its log showed repeated unclassified ALSA/cpal
backend errors and five internal device recoveries during startup. Distinguish
the backend's device recovery from the player's speaker ownership, diagnose the
remaining PCM glitch at its source, and rerun the lane several times on a loaded
host. Keep its existing derived timing and tone-quality bounds.

Public API: none expected. Wire: none.
