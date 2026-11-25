# [M] A lite-06 player paints a transcoded rung every run

## Goal

Scheduled Interop's `Portrait transcode` step passes every run: a lite-06 subscriber to a transcoded rung (`video/360p`) gets every group the relay's lite-07 copy holds and paints within the step's 30 s.

## Plan

Found by the Nightly triage of 2026-11-25 (scheduled Interop run [45469603959](https://github.com/moq-dev/moq/actions/runs/45469603959)). It also fails locally, 3 of about 85 runs, loaded and unloaded. In a failing run the transcoder claims, encodes, and finishes a 360p group every second, and the lite-06 Chromium player gets SUBSCRIBE_OK but decodes 0 frames. So groups are lost between the relay's lite-07 copy and its lite-06 subscriber, or inside the player. Ruled out: a rung rename (the rung keeps `video/360p` when the source's bitrate update re-resolves the ladder). Relay or transcoder debug logging stops it reproducing (0 of 47).

It first failed 2026-11-24, after lite-07 relay changes in `d2a4ba4a..d9d917aa`. Prime suspect: [moq-dev/moq#9536](https://github.com/moq-dev/moq/pull/9536) ("a pre-lite-07 tail owes nothing below SUBSCRIBE_START"), since the player is lite-06; others in range are #9444, #9531, #9534, #9523.

Decided 2026-11-25 (maintainer, /quest-plan): ✅ keep (rejected: drop). Recommended: first add cheap always-on accounting of groups the relay forwards to a lite-06 subscriber (cheap enough not to hide the race, and kept as the signal for next time), then bisect that range with about 40 loaded runs per point under a `stress-ng` loop.
