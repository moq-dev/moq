# [S] js/watch: prove inaudible steady-state audio convergence

## Goal

The "Real-time" (`"auto"`) preset plays with no audible glitch after
convergence, with the estimator's `QUANTILE` left at 0.95. Audio holds back or
catches up by playing slightly slower or faster, the way NetEq's preemptive
expand and accelerate do, rather than running dry, discarding buffered audio,
or rendering silence. Convergence is inaudible at ordinary drift and burst
sizes.

The maintainer's listening check of the native runs below settled the
audible part, and waived the live browser re-run. Every underrun and skip
ceiling, the nightly browser rows' included, is measured by
`test/audio-quality/budgets.json`'s rule, like every other budget, rather than
fixed at zero.

Boundaries: no packet loss concealment, so a frame that never arrives is still
missing audio; only received audio is stretched. The estimator stays as
`doc/concept/audio-jitter.md` specifies it.

## Plan

[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) ports the shared
NetEq stretch into the browser's ring, passing the shared PCM corpus. Both
players expand fast, as `doc/concept/audio-jitter.md` records: below the
target, every window, as many whole periods as fit, at the lower admission
threshold. Every replay row, `relay-mic-rtt40` included, measures and is
budgeted at 0 `player_underruns_per_min`. NetEq-exact expand, one period per
50 ms cooldown at the normal threshold, left `relay-mic-rtt40` at 2.1 and
`bursty-aac` at 2.2 player underruns a minute on the 2026-11-06 replays, and
is deleted.

What remains is the nightly browser ceilings, re-measured by `budgets.json`'s
rule from seven scheduled nightlies with the stretch. The lag heuristic behind
`skip_aheads` and `discarded_ms` reads a sustained accelerate as a skip, so
those can rise on rows whose target moves; `player_skips` counts the ring's
real re-anchors beside them. One local run of the twelve chromium rows on
2026-11-06 fit every current ceiling, with zero player underruns and skips.

Decided 2026-11-07 (maintainer, /quest-unblock on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058)): keep fast
expand. The maintainer listened to the native runs in both variants and could
not tell them apart, and left the pick to the coordinator. Both were equal in
steady state, but NetEq-exact was the only variant with gaps: each of its two
shaped rows had 2 short gaps (12 to 119 ms) in the first 3 s, while the target
rose from 120 to 160 ms, which fits its slower expansion. Fast expand was
already the default, so nothing needed re-measuring or restoring. The live
browser rows (Chrome at +40 ms, Safari on the direct path) were not run; the
maintainer waived them. Options shown:

- ✅ Fast expand
- NetEq-exact expand

The runs: macOS, head `354e35df40`, `moq play --video off --metrics`, `say`
speech from `LICENSE-APACHE` over Opus at 128 kb/s through ffmpeg
`-readrate`, relay `test/audio-quality/relay.toml`, shaper `bursty` seed 7,
120 s a row. Skips are not in the native metrics, so they are not counted.
Caveat: the output device changed between the sets (fast at 245 ms output
latency, exact at 13 ms), which confounds the startup comparison.

| Rate | Path | Variant | Underruns | Delay | Media / render | Gaps |
| --- | --- | --- | --- | --- | --- | --- |
| 1.002 | direct | fast | 0 | 120-140 ms | 1.0023 | 0 |
| 1.002 | bursty | fast | 0 | 120-260 ms | 1.0014 | 0 |
| 0.998 | direct | fast | 0 | 120 ms | 0.9982 | 0 |
| 0.998 | bursty | fast | 0 | 120-260 ms | 0.9974 | 0 |
| 1.002 | direct | exact | 0 | 120 ms | 1.0021 | 0 |
| 1.002 | bursty | exact | 0 | 120-260 ms | 1.0016 | 2 (startup) |
| 0.998 | direct | exact | 0 | 120-140 ms | 0.9982 | 0 |
| 0.998 | bursty | exact | 0 | 120-280 ms | 0.9979 | 2 (startup) |

Decided 2026-11-06 (maintainer, /quest-complete on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058)): the Goal was
wrong to require exactly zero steady-state player underruns, which pushed
expand away from NetEq in both players. The Goal is now no audible glitch in
steady state, judged by listening, with underrun ceilings measured by
`budgets.json`'s rule. Options shown:

- ✅ Audible, not zero
- Keep zero, and take fast expand in both players
- Keep NetEq-exact, and plan concealment of late audio as a separate quest

Decided 2026-11-06 (maintainer, /quest-unblock): drop the Safari row with
40 ms or more of added RTT. Safari can't be redirected through the RTT relay,
and shaping it needs Network Link Conditioner or `dnctl`/`pfctl` on the
laptop, so its rows run on the direct path. Not chosen: a hands-on Safari run
with shaped RTT.

Decided 2026-11-05 (maintainer, /quest-plan): steady-state lip sync comes
from this stretch converging the ring's playhead on `Sync.now()`, the shared
clock on local time anchored at the earliest arrival. The clock never follows
audio: one slow track must not hold the others, so audio and video stay in sync
in the steady state and drift apart only while a track is congested.
[moq-dev/moq#7941](https://github.com/moq-dev/moq/pull/7941) dropped an
audio-driven clock for this. Recommendation: measure the gap against the sample
reaching the output, through `AudioContext.getOutputTimestamp()`, so output
latency, Bluetooth included, is part of the lip sync.

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

## Waiting

- moq-dev/moq: seven scheduled Nightly runs on `main` with the stretch. Then
  re-measure the chromium ceilings in `test/audio-quality/budgets.json` from
  their `audio-quality-run` artifacts by the note's rule, adding
  `player_skips_per_min`.

## Related

- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - re-measures the AAC browser budgets after the publisher change, no gate on this proof
- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native time-stretch sharing one NetEq algorithm and its test vectors with this one
- [Buffered converge](/quest/m1/audio-jitter-target/buffered-converge.md) - reuses this stretch for buffered playback, which plays unstretched here
- [Watch worker](/quest/m3/watch-worker.md) - moves the worklet this stretch changes
