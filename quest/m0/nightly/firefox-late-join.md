# [M] A Firefox late join reaches live playback

## Goal

A Firefox player joining a live broadcast catches up within the media harness's derived playback deadline.

## Plan

[The Firefox media job](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122851001378) passes replacement,
fallback, and the negative controls, but its normal late join fails:
"held a frame 300ms behind the fixture 2000ms after the first frame, against
a 1500ms bound for playback". The `media-firefox` artifact retains traces.

Reproduce with the focused late-join case and identify whether reception,
selection, decoding, or presentation holds the player behind live. Preserve
the harness's derived deadline and its lagging-latecomer negative control.
This is separate from the IETF22 replacement/fallback failure already tracked
in `/quest/m0/test-flakes-2/ietf22-fallback.md`.

Public API: none expected. Wire: none expected.
