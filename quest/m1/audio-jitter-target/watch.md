# [L] js/watch: zero steady-state audio underruns by time-stretching the ring

## Goal

On the measured real-microphone rows, the "Real-time" (`"auto"`) preset plays
with zero player underruns after convergence and no skip-aheads in steady
state, with the estimator's `QUANTILE` left at 0.95. Audio holds back or
catches up by playing slightly slower or faster, the way NetEq's preemptive
expand and accelerate do, rather than running dry, discarding buffered audio,
or rendering silence. Convergence is inaudible at ordinary drift and burst
sizes.

The measured rows are the `relay-mic-rtt40` replay row, whose
`player_underruns_per_min` must reach 0, and a live re-run of the 2026-11-03
rows below on Chrome and Safari with a real microphone and 40 ms or more of
added RTT, read from the stats panel's audio Underruns.

Boundaries: no packet loss concealment, so a frame that never arrives is still
missing audio; only received audio is stretched. The estimator stays as
`doc/concept/audio-jitter.md` specifies it. Native playback is out of scope
here.

## Plan

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7679](https://github.com/moq-dev/moq/pull/7679)): pull time
stretch into m1 as this quest's fix, keeping `QUANTILE` at 0.95. The options
were restating the Goal to the measured rate, or raising the quantile. The m2
Time stretch quest was folded in here.

### Baseline, 2026-11-03

An agent ran the maintainer's MacBook Air (macOS 26.6.2) against
`cdn.moq.dev/anon`. The built-in microphone was published by `<moq-publish>` in
Chrome 156 and played by the harness page in a second Chrome 156 profile and in
Safari 26.6.2. The added RTT was a userspace UDP relay on the laptop holding
each packet 20 ms each way, reached through Chrome's `--host-resolver-rules`.
Safari cannot be redirected that way and negotiated WebSocket, so its rows ran
on the direct path. Underruns are the stats panel's counter after the first
30 s.

| Row | Transport | RTT min | Underruns after 30 s | Skip-aheads after 30 s | Auto delay p50 / max |
| --- | --- | --- | --- | --- | --- |
| Chrome, direct, 20 ms groups | WebTransport | 5 ms | 9 in 88 s (6.1/min) | 0 | 40 / 60 ms |
| Chrome, +40 ms, 20 ms groups | WebTransport | 47 ms | 26 in 149 s (10.5/min) | 1 (43 ms) | 100 / 120 ms |
| Chrome, +40 ms, 100 ms groups | WebTransport | 47 ms | 9 in 149 s (3.6/min) | 0 | 100 / 120 ms |
| Safari, direct, 20 ms groups | WebSocket | 5 ms | 5 in 118 s (2.5/min) | 0 | 120 / 140 ms |
| Safari, direct, 100 ms groups | WebSocket | 5 ms | 12 in 119 s (6.1/min) | 0 | 120 / 140 ms |

The Chrome +40 ms, 20 ms-group capture is checked in as the `relay-mic-rtt40`
replay row, budgeted exactly as measured: 7.9 player underruns a minute. Every
replay row now also budgets `player_underruns_per_min`, the player's own
counter, which counts the run-dry re-stalls the output tap files as stalled
time.

Replayed, each row's arrivals reproduce its live re-stalls and short quanta
almost exactly, so the browser's decode and render path adds nothing. The ring
sits at the target less one frame, and each run-dry is one frame arriving later
than that cushion. The target is the 95th percentile of each 500 ms
interval's worst delay, so about 5% of intervals, some six a minute, hold such
a frame. Raising `QUANTILE` in a replay sweep cut the rate but not to zero:
0.99 gave 1.0 to 3.5 a minute at a 60 to 200 ms p50 target, against 5.6 to
13.2 at 40 to 120 ms.

### How deep the stretch must go

Against the cushion each frame met, the five rows' late frames (12 to 27 in
each after 30 s) exceed it by a median of 10 to 16 ms and at most 44 to 81 ms.
Stretching the cushion across that wait means playing at `1 - k` of real time
from the moment the ring dips, with `k = excess / delay`. That is 11 to 31% at
the median, 30 to 47% at p90, and up to 38 to 71%. A bound of a few percent
per quantum cannot meet the Goal.

### Implementation: copy NetEq

Decided 2026-11-03 (maintainer): copy what WebRTC's NetEq does. The stretch
removes or repeats whole pitch periods of received audio, found by
autocorrelation, and cross-fades across the seam. It is not a generic
resampler or a phase vocoder. The mirrored files are in WebRTC's
`modules/audio_coding/neteq/`, read at `a057d00e46` (2026-11-03):

- `time_stretch.cc` and `time_stretch.h`, the shared search. Downsample to
  4 kHz and autocorrelate over lags `kMinLag` 10 to `kMaxLag` 60 (2.5 to 15 ms
  pitch periods), over `kCorrelationLen` 50. Then refine the best peak at full
  rate (`DspHelper::PeakDetection`), and normalise the cross-correlation by
  the two segments' energies. `SpeechDetection` treats a segment as passive
  when its energy per pitch sample is at most 8 times the background noise's
  (a fixed 75000 before noise is estimated). Passive audio may stretch
  regardless of correlation.
- `accelerate.cc`. With at least 30 ms of input, keep the first 15 ms,
  cross-fade one pitch period onto it, and drop that period. It stretches only
  when the correlation is over `kCorrelationThreshold` (0.9 in Q14) or the
  audio is passive. Fast accelerate lowers the bar to 0.5 and removes as many
  whole periods as fit in 15 ms.
- `preemptive_expand.cc`. Under the same 0.9 test, with at most 15 ms of old
  data ahead of the seam, repeat one pitch period after the unmodified part
  and cross-fade it in.
- `decision_logic.cc` and `decision_logic.h`, when to stretch. The low limit
  is the target and the high limit is the target plus the arrival history's
  maximum delay plus `kDelayAdjustmentGranularityMs` (20 ms).
  - Preemptive expand when the playout delay is under the low limit.
  - Accelerate at or above the high limit.
  - Fast accelerate at four times the high limit.
  - At most one stretch every `kMinTimescaleInterval` (5) ticks of 10 ms.

  Here the target is `Sync`'s resolved delay, and the arrival history's
  maximum is the estimator's.

NetEq decides once per 10 ms of output, and our worklet renders 128-sample
quanta. So the stretch runs on blocks of decoded PCM before they reach the
render quantum, where at least 30 ms are buffered.

One boundary differs from NetEq. NetEq preemptive-expands only while the next
packet is already buffered (`ExpectedPacketAvailable`). Once it is missing,
NetEq runs Expand, which is concealment and outside this quest. At NetEq's
cadence, one period of at most 15 ms every 50 ms, preemptive expand slows
playback by about a quarter at most. That covers the median late frame above
but not the 38 to 71% tail. So the work is: start at NetEq's thresholds,
measure `relay-mic-rtt40`, and only then decide how to close the tail. Options
are expanding sooner or more often as the ring nears empty, or accepting the
remaining re-stalls. Each departure from NetEq's constants is recorded with
the replay numbers that justify it.

- Put the stretch where `js/watch/src/audio/replay.ts` exercises it, such as
  `AudioRingBuffer`'s read path or a module the render worklet and the replay
  share, so the replay rows measure the stretch and not a model of it. Port
  the search with unit tests against NetEq's behaviour on synthetic periodic
  and noise signals.
- Accelerate replaces the skip-ahead when the ring sits above the high
  limit. A re-stall remains for a ring that empties despite the stretch, and a
  skip-ahead remains only above what fast accelerate can close.
- Verification: `relay-mic-rtt40` reaches 0 `player_underruns_per_min` and its
  budget is tightened to the new measurement. No other replay row regresses,
  and the nightly browser rows are re-measured by the budgets note's rule. A
  listening check confirms that a 2 ms/s drift and the measured expansions are
  inaudible or close to it. The 2026-11-03 Chrome and Safari +40 ms rows are
  re-run on real hardware.

### Publisher, measured in the same run

`AudioEncoder` input-to-output lag on live input is p50 0.9 ms and p99 1.1 ms,
with drift under 0.04 ms/s at 20 ms and 100 ms groups. The 7.35 s lag and 88
to 275 ms/s drift of
[moq-dev/moq#3477](https://github.com/moq-dev/moq/issues/3477) did not recur.
Each demand session opens with up to 30 s of retained history
(`js/publish/src/audio/backlog.ts`), which the watcher discards. 100 ms groups
halved Chrome's rate at +40 ms but more than doubled Safari's on the direct
path, so the 20 ms default stays.

Decided 2026-10-15: keep this proof in m1 with the existing playback and
A/V clock work.

## Waiting

- Maintainer: Safari with 40 ms or more of added RTT, the baseline row the
  2026-11-03 run could not shape. On the MacBook Air (`kixtop`), install
  Network Link Conditioner (Additional Tools for Xcode) or allow
  `sudo dnctl`/`pfctl`, then play `cdn.moq.dev/anon` in Safari with the stats
  panel open for two minutes, and record its audio Underruns after 30 s and the
  latency tab's auto delay.

## Required

- [A/V clock](/quest/m1/av-clock.md) - the clock the stretch converges toward

## Related

- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - confirms the build-flush fix on the nightlies, no gate on this proof
- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native time-stretch sharing one NetEq algorithm and its test vectors with this one
- [Watch worker](/quest/m2/watch-worker.md) - moves the worklet this stretch changes
