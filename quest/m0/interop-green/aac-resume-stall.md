# [M] The JS player resumes AAC audio from a Rust publisher

## Goal

Scheduled Interop's full-matrix `rust -> js` AAC (`mp4a`) case passes `waiting for resumed playback` every run: after resume, audio plays again alongside video.

## Plan

Found by the Nightly triage of 2026-11-25 (scheduled Interop run [45469603959](https://github.com/moq-dev/moq/actions/runs/45469603959)). After resume, video kept playing (899 frames) while audio stayed stalled: `audioTimestamp:0`, `rms:0`, about 9 KB received. Seen once; the old test-flakes epic that owned browser `rust -> js` timeouts was deleted by [moq-dev/moq#9573](https://github.com/moq-dev/moq/pull/9573).

Decided 2026-11-25 (maintainer, /quest-plan): ✅ keep under interop-green (rejected: under test-flakes-4; drop). Measure the failure rate on the focused case first, then find where the audio stops: the relay's audio track after resume, the JS AAC decoder's reset, or the player's audio clock.
