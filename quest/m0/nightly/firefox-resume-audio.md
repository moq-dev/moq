# [M] Firefox audio resumes with the picture

## Goal

Nightly's `Media (firefox player)` job passes its `after resume` window: after
a pause, audio is audible within the window's first samples, as in Chromium,
instead of staying silent for about half a second after the picture resumes.

## Plan

[moq-dev/moq#8635](https://github.com/moq-dev/moq/pull/8635) removed the
1.7 s trail behind the picture, but Nightly
[43601796176](https://github.com/moq-dev/moq/actions/runs/43601796176) (at
4fec611d4, after it and the [lag rule](https://github.com/moq-dev/moq/pull/8722))
still failed with `68/79 samples carried the fixture tone` (11-13: 60/78). The
window's first 11 samples are silent with the audio byte counter flat
(`audio=94041B`) while video presents live frames; then about 1.8 KB of
audio lands in one burst, `[player] skipping missing groups: track=audio
1165 -> 1258` fires, and audio plays in sync. So the audio subscription's
first groups reach the Firefox player about 0.4 s after video's.

Not reproduced locally: `just test media --media-engines firefox
--media-cases rename,pause` passed 4 of 4 on an idle host (2026-11-14). The
CI host was 14 to 60% idle during the window, and Firefox dials the relay
over WebSocket, so the difference is likely in that path under load.

Recommended: reproduce under load first (the job's own
`busiest host processes` table shows the shape), with the relay's and
player's logs kept (`HARNESS_KEEP=1`), and find which hop holds audio's
first groups after the resubscribe: the relay's upstream re-subscribe and
gap backfill (the fixture logs a fetch of audio group 1156 that the relay
aborts), the WebSocket session's stream scheduling, or the consumer's hole
wait from its pre-pause cursor (1165). Fix that hop; do not widen the test's
window or agreement.
