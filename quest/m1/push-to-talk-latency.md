# [M] Push-to-talk's press-to-audible latency fits a budget on the defaults

## Goal

On an ideal path with the browser defaults (`<moq-watch>` `delay: "auto"`,
20 ms Opus), push-to-talk's press-to-audible latency, excluding device output,
stays under a budget set from a real-device measurement on `kixtop`, and
`just test push-to-talk` enforces it with the per-part breakdown. Native
playback is out of scope.

## Plan

Decided 2026-11-04 (maintainer, planning in
[moq-dev/moq#7816](https://github.com/moq-dev/moq/pull/7816)):

- ✅ Budget the default path with realistic settings, not the fixed 300 ms /
  60 ms setup [moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760)
  measured; drop the fixed-delay row from `just test push-to-talk` (once Watch
  lands, per the 2026-11-06 decision below). The maintainer is fine assuming the constants are the problem.
- ✅ Measure capture and output latency on real macOS hardware (`kixtop`)
  first; set the budget from those numbers, and keep output-side knobs until
  then.
- ✅ Browser only.
- The catalog toggle forced at least a round trip of jitter buffer, so the
  budget is measured on silence suppression, which
  [moq-dev/moq#7942](https://github.com/moq-dev/moq/pull/7942) landed.

Decided 2026-11-06 (maintainer, on
[moq-dev/moq#8009](https://github.com/moq-dev/moq/pull/8009)): how should
`just test push-to-talk` run until Watch lands, since its no-underrun check
fails on the defaults by design?

- ✅ (a) Require Watch's time stretch ([moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058)) for this
  quest. The run holds a fixed 300 ms delay until then, and the defaults are
  budgeted after Watch lands, on the jitter buffer users will get.
- (b) Run the defaults now and stop failing on underruns while `"auto"` is
  on.
- (c) Run both rows, roughly doubling the nine-minute nightly step.

[moq-dev/moq#8009](https://github.com/moq-dev/moq/pull/8009) landed the
analyzer's delay timeline: the page records the resolved delay over time, so
each press's `jitter_buffer` ends at the first PCM that reaches the target the
ring held when that PCM landed. The run also keeps 60 ms / 128 kbit/s Opus,
the setup the nightly passes on. On the loaded agent host, 2026-11-06 runs at
the fixed 300 ms delay had no underrun or stall but lost heads ("utterance
head was not decoded"): 2 of 22 on the default 20 ms Opus and 1 on 60 ms.
The group after a head could overtake it, and the read took the mute's
timestamp jump as proof the head was too late to wait for;
[moq-dev/moq#8123](https://github.com/moq-dev/moq/pull/8123) fixed it, and two
20 ms runs after the fix kept all 44 heads. What remains is to switch the
page to the defaults, measure on `kixtop` again, and set and enforce the
budget.

Measured 2026-11-05 on the defaults, before Watch, one run of 22 presses
each, Chromium 156 headless through a local relay. `kixtop` is a MacBook Air
on macOS 26 with its built-in speakers as the default output. Linux is the
agent host under heavy load, with PipeWire output. The table gives the median
and max in ms over the separated presses with a breakdown: 15 on `kixtop`, 13
on Linux, which lost two heads.

| Part | `kixtop` | Linux |
| --- | --- | --- |
| capture | 9 / 12 | 10 / 11 |
| packetization | 16 / 20 | 16 / 20 |
| encode | 0.5 / 0.6 | 0.5 / 8 |
| network | 2 / 12 | 3 / 5 |
| decode | 0.5 / 3 | 0.6 / 17 |
| jitter buffer | 21 / 59 | 40 / 43 |
| render | 23 / 34 | -1 / 24 |
| press to render onset | 73 / 110 | 71 / 95 |
| device | 54 / 57 | 105 / 113 |
| `"auto"` target | 40 (max 80) | 60 |
| underruns / stalls | 10 / 9 | 64 / 33 |

Both runs failed the no-underrun check. `"auto"` targets the 0.95 quantile of
arrival lateness and accepts the re-stalls past it by design until Watch
time-stretches the ring. Push-to-talk's capture spread on this ideal path
reached 60 ms on `kixtop` and 78 ms on Linux, past a 40 or 60 ms target.
Linux's much higher count is not separated from its host load (load average
above 20) or its 43 ms render callbacks draining a 60 ms target.

Measurement limits: the harness's headless Chromium mutes its output and reads
a fake microphone, so `device` is Chromium's projected output for the default
device and `capture` excludes the microphone's own input latency. Linux's
render callbacks are 43 ms, so its `render` can be negative by up to one
callback. The lost heads were the ordering defect
[moq-dev/moq#8123](https://github.com/moq-dev/moq/pull/8123) fixed.

Knobs: the target's frame term (`frameDuration`, 20 ms by default) and its
measured term (`js/watch/src/audio/latency.ts`); device and render follow the
watch AudioContext's `latencyHint` (`"interactive"` in
`js/watch/src/audio/decoder.ts`), otherwise the platform.

## Related

- [Browser ceilings](/quest/m1/audio-jitter-target/browser-ceilings.md) - `"auto"` underruns less by time-stretching the ring, so the run can hold the default delay; the stretch landed in [moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058), and its nightly ceilings are re-measured there
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the `"auto"` target this latency feeds
