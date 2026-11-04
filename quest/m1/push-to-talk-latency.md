# [S] Push-to-talk's press-to-audible latency is chosen, not inherited

## Goal

Each part of a push-to-talk press's latency on an ideal path, from capture to
device output, is either reduced through the knob that controls it or kept
with a recorded reason. `just test push-to-talk` reports the result with the
same breakdown.

## Plan

Measured 2026-11-04 by
[moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760), for
[Push-to-talk](/quest/m1/push-to-talk/README.md). The setup was `just test
push-to-talk` through a local relay, publisher and listener on separate
sessions, with no shaping or loss, 60 ms Opus at 128 kbit/s, and a fixed 300
ms `delay`. Chromium 156 ran headless on Linux. There were five runs of 22
presses each, 15 of them separated, so 75 separated presses in all; one press
lost its head (see Push-to-talk) and has no breakdown, leaving 74.
`summary.json` `path` splits each press at page-clock boundaries that sum
exactly to press → projected output.

| Part | Boundaries | Median ms | Max ms |
| --- | --- | --- | --- |
| capture | worklet delivery and framing | 29 | 110 |
| packetization | media the head frame still needed after the press | 39 | 60 |
| encode | encoder input to container write | 0.8 | 14 |
| network | container write to arrival, through the relay | 3.2 | 15 |
| decode | arrival to decoded PCM | 0.7 | 13 |
| jitter buffer | head's PCM to the first PCM that filled the delay | 252 | 268 |
| render | that PCM to the render onset | 21 | 54 |
| device | render onset to projected output | 115 | 119 |
| total | press to projected output | 469 | 576 |

The capture and packetization split is approximate, but their 66 ms median sum
is exact. Playout on top of one-way delivery was 261 ms median and 294 ms max
over all 75, inside the 300 ms the maintainer set.

Biggest contributors and their knobs:

- **Playout delay, about 300 ms.** The ring starts once it holds the delay in
  decoded PCM, counted from the head's first sample. That is why the jitter
  buffer plus packetization sum to about 291 ms. Knob: `<moq-watch>` `delay`.
  This test fixes it at 300 ms. In `"auto"`, the default, the target is
  `max(measured, advertised) + frame + catalog delay`
  (`js/watch/src/audio/latency.ts`).
- **Device output, about 115 ms.** This matches Chromium's declared
  `baseLatency + outputLatency` on this headless Linux host. Knob: the watch
  AudioContext's `latencyHint` (`"interactive"` in
  `js/watch/src/audio/decoder.ts`), otherwise the platform. A real device,
  such as `kixtop`'s macOS output, should be measured before acting on it.
- **Capture and render, about 52 ms together.** Both follow the AudioContext
  callback cadence on this host. The capture graph also delivers 60 ms frames
  in bursts, 25-109 ms apart. Knob: the capture AudioContext's `latencyHint`
  (`js/publish/src/audio/capture.ts`), otherwise the platform.
- **Opus frame, 60 ms.** This test chooses it; the publisher defaults to
  20 ms. Under a fixed delay it is absorbed by the delay. In `"auto"` it adds
  one frame to the target. Knob: the encoder's `frameDuration`.

No part looked like a bug. On an ideal path, encode and decode take about 1 ms
and the relay hop about 3 ms, and the head is not trimmed as decoder warmup.
The one lost head is an ordering defect, owned by
[Push-to-talk](/quest/m1/push-to-talk/README.md), not a latency part. The
fixed delay bypasses the jitter estimator. In `"auto"`, the 46 ms median
per-press arrival-lateness spread, from capture bursts rather than the
network, would make the measured term nonzero; that is the estimator working
as specified in [Audio jitter
target](/quest/m1/audio-jitter-target/README.md).

Open: decide which knob to turn first, and whether to measure on a real
device before deciding.

## Related

- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the `"auto"` target this latency feeds
