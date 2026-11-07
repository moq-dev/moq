# [L] moq play converges inaudibly by time-stretching

## Goal

`rs/moq-audio`'s playback converges on its playout target by time-stretching
received audio, as WebRTC NetEq does, instead of running its ring dry and
padding with silence or trimming buffered audio. `moq play`'s native rows in
`test/audio-quality` play with no audible glitch once the target settles,
with underrun ceilings measured by `budgets.json`'s rule rather than fixed at
zero, as decided 2026-11-06 in
[Watch](/quest/m1/audio-jitter-target/watch.md). The
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
for both languages. The browser half remains
[Watch](/quest/m1/audio-jitter-target/watch.md); neither implementation waits
for the other.

The port retains WebRTC's BSD notice and patent grant, 4 kHz analysis,
50-sample correlation at lags 10 through 59, 14746/16384 normal and
8192/16384 fast admission thresholds, passive-speech VAD, 20 ms decision
band, 4x fast threshold, and 60 ms startup / 50 ms successful-stretch
cooldowns.
[moq-dev/moq#8058](https://github.com/moq-dev/moq/pull/8058) replaces NetEq's
expand in both players with a fast expand, recorded in the shared spec:
below the target, expand ignores the cooldown and takes every whole period
that fits at the lower threshold. It also drains a declared end's tail
unstretched, as finishing does. Analysis samples NetEq's 48 kHz FIR on a virtual clock for other
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
  ceilings once their targets settle. Keep a failed or void row explicit and fix
  any implementation cause before calling this complete.
- Re-measure the provisional native budgets from seven scheduled nightly
  artifacts using `budgets.json`'s rule: worst clean run times 1.25, plus
  its documented native hiccup, rounded to two significant figures. A
  developer run is not that calibration.
- Run `just rs bench-playback` on a quiet host with no other agents. It
  now sweeps both sink count and callback size with and without stretch,
  and the nightly `rust-test` benchmark job (`sh/gh/nightly.sh`) already
  runs it. A loaded-host smoke run is not a timing acceptance claim.
- Listen to real speech with 2 ms/s drift and the measured expansions on
  a real output device (below). Deterministic PCM and allocation checks do
  not establish perceptual acceptance.

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
none in steady state. That predates the expand departure above, so
[Watch](/quest/m1/audio-jitter-target/watch.md)'s listening check repeats these
runs; the calibration and bench waits below remain.

## Waiting

- moq-dev/moq: collect seven scheduled native nightly rows for this
  implementation, then apply the existing calibration rule. Keep the
  current provisional budgets until those artifacts exist.
- moq-dev/moq: run `just rs bench-playback` on a quiet host. On
  2026-11-04 `nixos`, `nixos2`, and `kixtop` all ran agents and `desktop`
  refused SSH.

## Related

- [Watch](/quest/m1/audio-jitter-target/watch.md) - the browser stretch sharing this algorithm and its vectors
