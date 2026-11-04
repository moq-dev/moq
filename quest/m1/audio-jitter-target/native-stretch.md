# [L] moq play reaches zero steady-state underruns by time-stretching

## Goal

`rs/moq-audio`'s playback converges on its playout target by time-stretching
received audio, as WebRTC NetEq does, instead of running its ring dry and
padding with silence or trimming buffered audio. `moq play`'s native rows in
`test/audio-quality` reach zero underruns once the target settles. The
stretch shares one algorithm and one set of test vectors with the browser's.

Boundaries: no packet loss concealment, so a frame that never arrives is
still missing audio; only received audio is stretched. The estimator
(`rs/moq-audio/src/jitter.rs`) stays as `doc/concept/audio-jitter.md`
specifies it.

## Plan

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7679](https://github.com/moq-dev/moq/pull/7679)): copy NetEq in m1.
The implementation now ports its received-audio accelerate and preemptive
expand into the native playback callback. The estimator is unchanged.
`doc/concept/audio-jitter.md` specifies the shared floating-point algorithm;
`doc/concept/audio-jitter/stretch/corpus.json` supplies literal PCM vectors
for both languages. The browser half remains
[Watch](/quest/m1/audio-jitter-target/watch.md); neither implementation waits
for the other.

The port retains WebRTC's BSD notice and patent grant, 4 kHz analysis,
50-sample correlation at lags 10 through 59, 14746/16384 normal and
8192/16384 fast admission thresholds, passive-speech VAD, 20 ms decision
band, 4x fast threshold, and 60 ms startup / 50 ms successful-stretch
cooldowns. Analysis samples NetEq's 48 kHz FIR on a virtual clock for other
device rates and uses floating-point peak refinement and fades. Those
arithmetic adaptations are documented, not claimed bit-identical to int16
WebRTC. No threshold was tuned.

Callback state is preallocated and uses the existing SPSC ring. Startup
silence and short/finished input bypass stretching. A genuine underrun
emits emergency silence until received PCM refills the target; it never
reanalyses padding or repeats history after input ends. Only the existing
capacity watermark and an incoming excess at the fast threshold trim.

Remaining verification:

- Native `test/audio-quality` rows must show zero steady-state underruns
  once their targets settle. Keep a failed or void row explicit and fix
  any implementation cause before calling this complete.
- Re-measure the provisional native budgets from seven scheduled nightly
  artifacts using `budgets.json`'s rule: worst clean run times 1.25, plus
  its documented native hiccup, rounded to two significant figures. A
  developer run is not that calibration.
- Run `just rs bench-playback` on a quiet host with no other agents. It
  now sweeps both sink count and callback size with and without stretch
  and is included in nightly CI. A loaded-host smoke run is not a timing
  acceptance claim.
- Listen to real speech with 2 ms/s drift and the measured expansions on
  a real output device. Deterministic PCM and allocation checks do not
  establish perceptual acceptance.

Public API proposal: `playback::Sink::set_latency(Duration)` passes the
moving target from `moq play` to the callback and preserves it across device
rebuilds. Existing fixed-latency callers need no change. Wire: none.

## Waiting

- Maintainer: accept the proposed `Sink::set_latency(Duration)` API and
  documented floating-point NetEq adaptation, or request a different target
  handoff / an int16 port. Recommend the proposed setter and shared float
  corpus, consistent with the estimator's cross-language arithmetic. The
  quest expected no API addition; the callback otherwise knows only its
  opening target, while the CLI owns the moving estimate. The AFK brief
  reserves consequential API choices for the maintainer.
- Maintainer: confirm listening evidence for 2 ms/s drift and measured
  expansions on a real device. Recommend retaining this check before
  claiming the perceptual goal; automated amplitude tests cannot decide it.
- moq-dev/moq: collect seven scheduled native nightly rows for this
  implementation, then apply the existing calibration rule. Keep the
  current provisional budgets until those artifacts exist.

## Related

- [Watch](/quest/m1/audio-jitter-target/watch.md) - the browser stretch sharing this algorithm and its vectors
