# [M] Audio is placed on the clock where it is heard

## Goal

Each player compares audio with the shared clock at the moment the audio
leaves the speaker, so audio and video stay in sync on any output device, and
the audio target counts that output latency. The browser rests on the clock
as native does, without the underruns a clock pairing alone caused, and
native's fixed 50 ms floor, which raises the shared delay for video too, is
replaced by its device's actual latency.

## Plan

Planned 2026-11-11 (maintainer, /quest-merge pick of
[moq-dev/moq#8507](https://github.com/moq-dev/moq/pull/8507)) for the
browser's render callback. Widened 2026-11-13 (maintainer, /quest-plan):
✅ lag is measured where audio is heard, in both players; the
[epic](/quest/m2/audio-playout/README.md) records the decision.

Neither player reads its output latency today. The browser never reads
`baseLatency` or `outputLatency`. Natively `playback/driver.rs` places audio
by the host clock at the callback and hands the device's reported latency
only to the playback ledger, and `moq play` floors the audio target at 50 ms
(`AUDIO_BUFFER_MIN` in `play/media.rs`) instead. So audio is
heard later than video by the device latency: tens of milliseconds on a
desktop sink, 150 to 250 ms over Bluetooth.

The browser evidence: headless Chromium renders 2048-frame callbacks
(`baseLatency` 42.7 ms) and pairs the clock with `currentTime` just after
each one, so it rests 28 to 35 ms behind where native rests within a few ms;
Nightly's near-zero rows show +12 to +20 ms since #8507. A max-filtered
wall-clock pairing put audio on the clock (p50 -1.3 to 4.5 ms) but underran
near-zero Opus 45 times a minute, since the target did not count the
callback.

Recommendations, in one PR:

1. Add the output latency to each player's audio target: in the browser,
   `baseLatency` (or the measured callback size where it is missing) plus
   `outputLatency` where reported, and natively the device period plus the
   latency the host reports. The
   shared delay then covers it, so video waits that much longer, which lip
   sync on that device needs.
2. Read the clock at output time: in the browser, pair it on the wall clock
   with the max filter; natively, use the callback's playback timestamp.
3. Measure the live rows before and after in both players.
