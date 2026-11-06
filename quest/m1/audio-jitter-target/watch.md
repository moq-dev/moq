# [M] js/watch: prove zero steady-state audio underruns on real hardware

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
rows below with a real microphone, Chrome's with 40 ms or more of added RTT
and Safari's on the direct path, read from the stats panel's audio Underruns
and Skips.

Boundaries: no packet loss concealment, so a frame that never arrives is still
missing audio; only received audio is stretched. The estimator stays as
`doc/concept/audio-jitter.md` specifies it.

## Plan

[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) ports the shared
NetEq stretch into the browser's ring, passing the shared PCM corpus, and
every replay row, `relay-mic-rtt40` included, measures and is budgeted at 0
`player_underruns_per_min`. NetEq-exact expand left `relay-mic-rtt40` at 2.1
a minute, so expand departs from NetEq in both players, as
`doc/concept/audio-jitter.md` records: below the target it expands every
window, as many whole periods as fit, at the lower admission threshold. What
remains is proof outside the replay:

- The 2026-11-03 rows re-run on real hardware, Chrome's at +40 ms and Safari's
  on the direct path, under Waiting.
- A listening check of that run and of the native player, since the expand
  departure changes both and postdates the 2026-11-05 native listening in
  [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md).
- The nightly browser ceilings re-measured by `budgets.json`'s rule from seven
  scheduled nightlies with the stretch. The lag heuristic behind
  `skip_aheads` and `discarded_ms` reads a sustained accelerate as a skip, so
  those can rise on rows whose target moves; `player_skips` counts the ring's
  real re-anchors beside them. One local run of the twelve chromium rows on
  2026-11-06 fit every current ceiling, with zero player underruns and skips.

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

- Maintainer: re-run the 2026-11-03 rows on the MacBook Air and listen to
  them, since an agent has no microphone, speaker, or Safari. Automation
  considered: rendering speech through the ring offline, but the repository's
  only speech clips are 0.4 s, and the live run exercises the same stretch on
  a real voice. On a checkout of `main` once
  [moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) merges, or of
  its branch before:
  1. `just web serve https://cdn.moq.dev/anon`, and in its publish tab
     publish the built-in microphone with 20 ms groups, the default.
  2. For the +40 ms RTT, `cargo run --release -p moq-shaper -- --listen
     127.0.0.1:443 --target "$(dig +short cdn.moq.dev | head -1):443"
     --delay 20ms`, then open the watch page in a second Chrome profile:
     `open -na "Google Chrome" --args --user-data-dir=/tmp/rtt40
     --host-resolver-rules="MAP cdn.moq.dev 127.0.0.1"`.
  3. Play the broadcast at the Real-time delay and talk for three minutes.
     Note the stats panel's audio Underruns and Skips at 30 s and at the end,
     and listen for warble or pitch wobble.
  4. Repeat with 100 ms groups, then both group sizes in Safari on the direct
     path, with no shaper.
  5. Repeat the 2026-11-05 native listening runs of
     [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) on the
     same checkout: `say` speech over Opus at a 1.002 and a 0.998 publisher
     rate, direct and through the shaper's `bursty` profile.

  Done when every row reads zero Underruns and Skips between 30 s and the end
  and the stretch is inaudible or close to it. Record the table here.
- moq-dev/moq: seven scheduled Nightly runs on `main` with the stretch. Then
  re-measure the chromium ceilings in `test/audio-quality/budgets.json` from
  their `audio-quality-run` artifacts by the note's rule, adding
  `player_skips_per_min`, and check that the rows meet the Goal.

## Related

- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - re-measures the AAC browser budgets after the publisher change, no gate on this proof
- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native time-stretch sharing one NetEq algorithm and its test vectors with this one
- [Buffered converge](/quest/m1/audio-jitter-target/buffered-converge.md) - reuses this stretch for buffered playback, which plays unstretched here
- [Watch worker](/quest/m3/watch-worker.md) - moves the worklet this stretch changes
