# [M] js/watch: prove NetEq's audio time stretch on real hardware

## Goal

On the measured real-microphone rows, the "Real-time" (`"auto"`) preset
converges on its target the way WebRTC NetEq does, with the estimator's
`QUANTILE` left at 0.95: audio holds back or catches up by playing slightly
slower or faster, through NetEq's preemptive expand and accelerate and its
decision band, rather than discarding buffered audio. It plays with no audible
glitch after convergence, judged by the maintainer's listening check, and no
row skips buffered audio in steady state.

What still runs dry is what NetEq itself would conceal: a receive stall longer
than the target. The measured rows are the `relay-mic-rtt40` replay row, at
the 1.0 `player_underruns_per_min` NetEq measures there (7.9 on `main` without
the stretch), and a live re-run of the 2026-11-03 rows below with a real
microphone, Chrome's with 40 ms or more of added RTT and Safari's on the
direct path, read from the stats panel's audio Underruns and Skips beside
their 2026-11-03 baseline. Their underrun and skip ceilings are measured by
`test/audio-quality/budgets.json`'s rule, like every other budget, rather than
fixed at zero.

Boundaries: no packet loss concealment, so a frame that never arrives is still
missing audio; only received audio is stretched. The estimator's target stays
as `doc/concept/audio-jitter.md` specifies it.

## Plan

[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) ports the shared
NetEq stretch into the browser's ring, passing the shared PCM corpus, and adds
NetEq's maximum-delay term to the decision band in both players: the estimator
reports its `lateness`, how late the slowest frame of the last two seconds
arrived against the fastest, and the band above the target widens by it, as
NetEq's `DecisionLogic` adds `GetMaxDelayMs`. Without it `relay-mic-rtt40` ran
dry 2.1 times a minute; with it, 1.0. Every replay row is budgeted at what it
measures. The one difference from NetEq left, deciding on the ring's depth
rather than NetEq's playout delay, is recorded in the shared spec, and the PR
lists it as a follow-up. What remains is proof outside the replay:

- The 2026-11-03 rows re-run on real hardware, Chrome's at +40 ms and Safari's
  on the direct path, under Waiting.
- A listening check of that run. The native player's 2026-11-05 listening in
  [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) still
  holds: the band only changes when it stretches, not how.
- The nightly browser ceilings re-measured by `budgets.json`'s rule from seven
  scheduled nightlies with the stretch. The lag heuristic behind
  `skip_aheads` and `discarded_ms` reads a sustained accelerate as a skip, so
  those can rise on rows whose target moves; `player_skips` counts the ring's
  real re-anchors beside them.

Decided 2026-11-06 (maintainer, /quest-complete on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058)): the Goal was
wrong to require exactly zero steady-state player underruns, which pushed
expand away from NetEq in both players. The Goal is now no audible glitch in
steady state, judged by listening, with underrun ceilings measured by
`budgets.json`'s rule. Options shown:

- ✅ Audible, not zero
- Keep zero, and take fast expand in both players
- Keep NetEq-exact, and plan concealment of late audio as a separate quest

Decided 2026-11-06 (maintainer, on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058)): start with
NetEq, which settles the expand the decision above left to the listening
check; the fast variant and its `FAST_EXPAND` switch are gone. Zero underruns
is aggressive, and a +40 ms RTT alone should cost NetEq nothing, so the port
was checked against `decision_logic.cc` and `preemptive_expand.cc` first. A constant RTT does cost nothing: the delays are
measured against the fastest arrival. What runs `relay-mic-rtt40` dry is
receive stalls of 100 to 125 ms whose frames then land together and out of
order, behind an 80 to 100 ms target, the 0.95 quantile of half-second peaks
that NetEq's estimator reads too; NetEq would conceal those. Stalls over 60 ms
come 13 times a minute there against 3.4 on the direct row. The port had dropped the
maximum-delay term, now restored. Not chosen: the PR's earlier stronger
expand, every window at the lower threshold, which reached zero on every
replay row at the cost of a stronger stretch while the ring is short.

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7679](https://github.com/moq-dev/moq/pull/7679)): pull time
stretch into m1 as this quest's fix, keeping `QUANTILE` at 0.95. The options
were restating the Goal to the measured rate, or raising the quantile. The m2
Time stretch quest was folded in here.

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

The Chrome +40 ms, 20 ms-group capture is checked in as the `relay-mic-rtt40`
replay row: 7.9 player underruns a minute on `main`, 1.0 with the stretch.

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

  Done when the stretch has no audible glitch in steady state and every row
  reads zero Skips between 30 s and the end. Record the table of Underruns and
  Skips per row here, beside the baseline.
- moq-dev/moq: seven scheduled Nightly runs on `main` with the stretch. Then
  re-measure the chromium ceilings in `test/audio-quality/budgets.json` from
  their `audio-quality-run` artifacts by the note's rule, adding
  `player_skips_per_min`, and check that the rows meet the Goal.

## Related

- [Capped budgets](/quest/m1/audio-jitter-target/capped-budgets.md) - re-measures the AAC browser budgets after the publisher change, no gate on this proof
- [Native stretch](/quest/m1/audio-jitter-target/native-stretch.md) - the native time-stretch sharing one NetEq algorithm and its test vectors with this one
- [Buffered converge](/quest/m1/audio-jitter-target/buffered-converge.md) - reuses this stretch for buffered playback, which plays unstretched here
- [Watch worker](/quest/m3/watch-worker.md) - moves the worklet this stretch changes
