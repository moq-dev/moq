# [L] moq play converges inaudibly by time-stretching

## Goal

`rs/moq-audio`'s playback converges on its playout target by time-stretching
received audio, as WebRTC NetEq does, instead of running its ring dry and
padding with silence or trimming buffered audio. `moq play`'s native rows in
`test/audio-quality` play with no audible glitch once the target settles,
with underrun ceilings measured by `budgets.json`'s rule rather than fixed at
zero, as decided 2026-11-06 on
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058). The
stretch shares one algorithm and one set of test vectors with the browser's.

Boundaries: no packet loss concealment, so a frame that never arrives is
still missing audio; only received audio is stretched. The estimator
(`rs/moq-audio/src/jitter.rs`) stays as `doc/concept/audio-jitter.md`
specifies it.

## Plan

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7679](https://github.com/moq-dev/moq/pull/7679)): copy NetEq in
m1. The implementation, merged 2026-11-04 in
[moq-dev/moq#7856](https://github.com/moq-dev/moq/pull/7856), ports its
received-audio accelerate and preemptive expand into the native playback
callback. The estimator is unchanged. `doc/concept/audio-jitter.md` specifies
the shared floating-point algorithm;
`doc/concept/audio-jitter/stretch/corpus.json` supplies literal PCM vectors
for both languages. The browser half landed in
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058).

The port retains WebRTC's BSD notice and patent grant, 4 kHz analysis,
50-sample correlation at lags 10 through 59, 14746/16384 normal and
8192/16384 fast admission thresholds, passive-speech VAD, 20 ms decision
band, 4x fast threshold, and 60 ms startup / 50 ms successful-stretch
cooldowns.
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) drains a declared
end's tail unstretched, as finishing does. Its browser half decides on the
ring's depth right after each arrival and never expands, since expanding on
the depth between arrivals settled audio a burst behind the shared clock
(decided 2026-11-08).
[moq-dev/moq#8361](https://github.com/moq-dev/moq/pull/8361) measured native
settling behind its clock the same way (A/V lag p50 up to 88 ms) and moves it
onto that decision, with an expand only for a publisher clock slower than the
device's.
Analysis samples NetEq's 48 kHz FIR on a virtual clock for other
device rates and uses floating-point peak refinement and fades. Those
arithmetic adaptations are documented, not claimed bit-identical to int16
WebRTC. No threshold was tuned.

Callback state is preallocated and uses the existing SPSC ring. Startup
silence and short/finished input bypass stretching. A genuine underrun
emits emergency silence until received PCM refills the target; it never
reanalyses padding or repeats history after input ends. Only the existing
capacity watermark and an incoming excess at the fast threshold trim.

Remaining verification:

- Native `test/audio-quality` rows must meet their measured underrun
  ceilings once their targets settle. Keep a failed or void row explicit and
  fix any implementation cause.
- Re-measure the provisional native budgets from seven scheduled nightly
  artifacts that carry `moq play`'s own `trimmed` and `accelerated`
  counters, using `budgets.json`'s rule: worst clean run times 1.25, plus
  its documented native hiccup, rounded to two significant figures. A
  developer run is not that calibration. Native rows then budget the
  player's own `player_skips_per_min` and `trimmed_ms_per_min` in place of
  the analyzer's `skip_aheads_per_min` and `discarded_ms_per_min`, which
  count the accelerate too.

Graded 2026-11-08: the scheduled Nightlies of 2026-11-05 to 11-08 on
`main` (42050683248, 42236107954, 42377867523, 42487375545) are the first
four with this implementation; 11-04's ran at 14:56 UTC, before it merged
at 17:31. Every night has all 12 native rows and none voided. Eleven rows
read zero underruns every night. Opus `step` reads one 22 to 44 ms gap on
three nights, at 29.6 s, when the shaper's one-way delay steps from 5 to
60 ms while the settled target is 60 ms: a late arrival no stretch can
cover without concealment, played clean once the target rises to 100 ms.
The scheduled Nightlies of 11-02 to 11-04, before this implementation,
read up to 23.9 underruns a minute on AAC `wide` and 8.6 on Opus `wide`.

The stretch adds skip-aheads the harness did not see before: up to 3.3 a
minute and 232 ms discarded on Opus `bursty`, 2.2 and 181 ms on AAC
`near-zero` and `step` on 11-08. Each is the accelerate shedding a lowered
target or a startup overfill at about 8% speed for 1 to 4 s, with no single
sample's media advancing more than 33 ms past real time. The harness counts
any shed of more than 40 ms across nine samples as a skip, which its README
now says. Two rows failed their provisional ceilings that way: Opus
`bursty` on 11-07 (232 > 170 ms discarded, and a 380 ms startup target hold
> 350) and AAC `step` on 11-08 (157.5 > 110). The rest of the job's red is
outside this work: `raw-audio-bench` on 11-06, `qmux-bench` on 11-08, and
browser and replay rows.

The nightly `rust-test` benchmark job ran `just rs bench-playback` and
passed on all four nights, alone on a GitHub `ubuntu-24.04-arm` runner. The
stretch costs 16 to 43% per callback, stable across nights; its worst cell,
64 sinks at 2048 frames, is 1.02 ms of a 42.7 ms period.
[moq-dev/moq#8308](https://github.com/moq-dev/moq/pull/8308)'s body keeps
every graded native row's metrics.

Decided 2026-11-09 (maintainer, on
[moq-dev/moq#8308](https://github.com/moq-dev/moq/pull/8308#issuecomment-6730322286)):

- ✅ `moq play --metrics` reports its own trimmed and accelerated audio,
  and the calibration budgets trims alone (rejected: keep budgeting
  accelerates as `discarded_ms`). An accelerate is inaudible, so it is not
  the discarded audio these rows guard. #8308 adds the counters to both
  players, beside the browser's skip count
  [moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) added.
- ✅ The nightly runner's `bench-playback` counts as the quiet-host run
  (rejected: naming another host). That settles the timing check.

Decided 2026-11-04 (maintainer, on
[moq-dev/moq#7856](https://github.com/moq-dev/moq/pull/7856)): accept the
additive `playback::Sink::set_latency(Duration)` and the shared
floating-point NetEq adaptation, with its documented arithmetic differences
from bit-exact int16 WebRTC. The setter passes the moving target from
`moq play` to the callback and preserves it across device rebuilds; existing
fixed-latency callers need no change. Wire: none. Options shown:

- ✅ The additive setter and the shared float adaptation
- An int16 port of WebRTC's DSP
- A different target handoff without the setter

The native Opus `bursty` target p95 of 360 ms that one developer run of this
implementation reported, over the 350 ms provisional ceiling, is a startup
interval rather than the stretch. Its target jumped from the 120 ms prior to
360 ms 2 s in and held until the 20 s half-life brought it to 180 ms at
23 s; the run's other two attempts read 180 ms. The reported target is the
estimator's (`consumer.delay()` in `rs/moq-cli/src/play/media.rs`), timed
when the receive loop reads each frame, and this work changes neither.
Seven interleaved runs each of `main` (`635dd54f3c`) and this branch on one
loaded 24-core host (load 13 to 55, seed 7, 60 s) gave target p95s of
140, 180, 180, 180, 160, 240, and 160 ms on `main` and 180, 200, 200, 180,
180, 180, and 180 ms here, both with a median of 180 ms. The worst was on
`main`, a second late interval at 7 s. Every run of this branch had zero
underruns; `main` had up to 14.1 a minute. The 2026-10-29 to 11-03 scheduled
nightlies on `main` read 160 to 280 ms. So the ceiling stays as it is,
as `test/audio-quality/budgets.json`'s note budgets startup holds as measured,
until the seven-nightly calibration below.

Listened 2026-11-05 (maintainer, /quest-unblock): the stretch is inaudible
in all four runs on `kixtop` at `main` `7e621623a8`, two minutes each of
`say` speech from `LICENSE-APACHE` over Opus 128 kb/s, the relay at
`test/audio-quality/relay.toml`, and the shaper's `bursty` profile, seed 7.
The player's metrics confirm the stretch was active: media per render second
was 1.0020 (fast, direct) and 1.0018 (fast, shaped) against a 1.002
publisher, and 0.9981 and 0.9979 against 0.998; the shaped runs raised the
target delay from 120 to 260 ms. Each direct run stalled once at startup,
none in steady state. This settles the perceptual check; the calibration and
bench waits below remain.

## Waiting

- moq-dev/moq: seven scheduled Nightlies on a `main` that includes
  [moq-dev/moq#8361](https://github.com/moq-dev/moq/pull/8361), whose
  native rows decide where audio arrives and report `av_lag_ms` beside
  [moq-dev/moq#8308](https://github.com/moq-dev/moq/pull/8308)'s
  `player_skips` and `trimmed_ms`; the calibration budgets `av_lag_ms` too. Find them with `gh run list --workflow
  nightly.yml --event schedule`, download each with `gh run download <id>
  -n audio-quality-run` within its 7 days, keep its native rows in the
  calibration PR's body, and apply the calibration rule once seven exist.
  Never dispatch the nightly by hand.

## Related

- [Browser ceilings](/quest/m1/audio-jitter-target/browser-ceilings.md) - re-measures the browser rows with the stretch sharing this algorithm and its vectors
