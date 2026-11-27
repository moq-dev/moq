# [M] The JS player resumes AAC audio from a Rust publisher

## Goal

Scheduled Interop's full-matrix `rust -> js` AAC (`mp4a`) case passes `waiting for resumed playback` every run: after resume, audio plays again alongside video.

## Waiting

- moq-dev/moq: a scheduled Interop run fails `rust -> js` on `waiting for resumed playback` again, now printing the diagnostics below. Check each day's run with `gh run list --repo moq-dev/moq --workflow interop.yml --event schedule`, then `grep -c 'waiting for resumed playback'` in its `Interop` job log; the `interop-failure` artifact holds `relay.log`, `pub-rust.log`, and `rust-js.log`.

## Plan

Found by the Nightly triage of 2026-11-25 (scheduled Interop run [45469603959](https://github.com/moq-dev/moq/actions/runs/45469603959), at d9d917aa5d). After resume, video kept playing (899 frames) while audio stayed stalled: `audioTimestamp:0`, `rms:0`, about 9 KB received.

Decided 2026-11-25 (maintainer, /quest-plan): ✅ keep under interop-green (rejected: under test-flakes-4; drop). Measure the failure rate on the focused case first, then find where the audio stops.

Measured 2026-11-26 ([moq-dev/moq#9664](https://github.com/moq-dev/moq/pull/9664)):

- CI: 1 of 56 `rust -> js` cells failed over the 14 scheduled runs from 2026-11-12 to 2026-11-25 (four versions per run), only that once.
- Locally: 0 of 271 focused runs (one relay, a fresh fMP4 Rust publisher per run, the JS driver for opus then mp4a). They covered a publisher aged 15 s, everything pinned to 4 CPUs on a loaded host, the publisher switching from WebSocket to WebTransport mid-run (`MOQ_CONNECT_WEBSOCKET_DELAY=0`), and the publisher frozen 1.5 s to republish its catalog. 60 of the switching runs were at the failing head, d9d917aa5d, so the switch alone reproduces nothing there either. No differential, so nothing to bisect.

What the failing run shows:

- The audio never reached the decode loop after resume. `audioBytes` counts each frame the loop reads, before it decodes, and it stayed at the paused value for 30 s. So the decoder, ring, and clock never saw a frame. Either nothing arrived from the relay, or the reader (the ordered read the decoder takes audio from) held what did.
- The publisher's first dial lost QUIC's 200 ms head start on the loaded runner. It ran over WebSocket for 8 s, then switched to WebTransport (`leaving the WebSocket fallback`). The relay moved the broadcast to the new session (conn 12) 240 ms after the mp4a page joined, between its first audio subscribe and its pause. On resume the relay subscribed video and audio upstream on conn 12, and both stayed open until teardown. 9 of the local runs put the switch in that same window and passed.
- The page received the catalog three more times after resume, without resubscribing. The opus page before it received it once. Locally a catalog update came only from a frozen publisher, and never stalled audio.

So a recurrence decides it. The PR that measured this makes the failure print more: the page's audio buffered ranges (`audioBuffered`), its rendition error (`audioError`), and the catalog's audio `delay` and `jitter` (`audioCatalogDelay`, `audioCatalogJitter`). The relay also logs each downstream subscription's request (`moq_net::lite::publisher=debug`). Next time:

- `audioBuffered` empty: nothing arrived. Read the relay's downstream request for `2.mp4-a` against its upstream subscription on the new route.
- `audioBuffered` past the paused playhead: audio arrived and the JS ordered read held it. Look at its gap or time-start wait.

The net fixes that landed after d9d917aa5d may already cover it, notably [moq-dev/moq#9575](https://github.com/moq-dev/moq/pull/9575) (a superseded epoch answering a fresh request; a publisher reconnect is a new epoch) and [moq-dev/moq#9564](https://github.com/moq-dev/moq/pull/9564) (reader wakeups, a replaced copy's drain).

Moved 2026-11-26 (maintainer, pre-0.22.0 audit on [moq-dev/moq#9703](https://github.com/moq-dev/moq/pull/9703)) out of interop-green to `m0` so its two-week wait does not hold the release cut, which supersedes keeping it under interop-green.

Decided 2026-11-26 (maintainer, /quest-run on [moq-dev/moq#9664](https://github.com/moq-dev/moq/pull/9664)): ✅ land the diagnostics, including the relay's per-subscription request logging, and keep this quest waiting for the next failure; delete it once two weeks of scheduled runs (56 `rust -> js` cells) pass without one (rejected: delete it now as fixed by #9575 and #9564; land only the player-side fields).

## Related

- Rejoin replay, deleted in [moq-dev/moq#9620](https://github.com/moq-dev/moq/pull/9620) - lite-06 audio across a relay rejoin in the Firefox media suite; a different test and cause (a lingering copy's resubscribe ask), not this one
