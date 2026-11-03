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
[moq-dev/moq#7679](https://github.com/moq-dev/moq/pull/7679): "m1 please copy
what NetEQ does"), milestone:

- m0
- ✅ m1, in this epic
- m2

This is the native half. The browser half is
[Time stretch](/quest/m2/watch-audio-time-stretch.md) on `main` today;
#7679, still open, folds it into
[Watch](/quest/m1/audio-jitter-target/watch.md) in m1 and scopes native
playback out of Watch. Wherever the browser stretch lives when this starts,
it shares the algorithm and vectors below.
Today the native ring (`rs/moq-audio/src/ring.rs`) pads a dry ring with
silence back up to its latency and trims when the writer runs ahead, so
jitter and drift cost a glitch.

**Copy NetEq.** Accelerate removes, and preemptive expand repeats, whole pitch
periods found by autocorrelation, overlap-adding at the seam. Start from
NetEq's thresholds and change one only with a measurement:

- [`time_stretch.h`](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/time_stretch.h) and
  [`time_stretch.cc`](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/time_stretch.cc): the shared search. It downsamples
  to 4 kHz, autocorrelates over `kCorrelationLen` 50 samples for lags
  `kMinLag` 10 to `kMaxLag` 60 (2.5 to 15 ms), runs a simple VAD, and
  stretches only on `kCorrelationThreshold` 14746 (0.9 in Q14) or passive
  speech.
- [`accelerate.cc`](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/accelerate.cc): about 30 ms of input, and a 0.5
  threshold (8192 in Q14) in fast mode.
- [`preemptive_expand.cc`](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/preemptive_expand.cc): the 0.9 threshold and
  at least 15 ms of new data.
- [`decision_logic.cc`](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/decision_logic.cc): when to accelerate or expand
  against the target, at `kDelayAdjustmentGranularityMs` (20 ms).

WebRTC is BSD-3-Clause; keep its notice with any code ported from it.

**Share with JS.** Like the estimator, the stretch is specified once and
implemented in both languages. Whichever of this quest and the browser stretch lands first
adds the stretch to `doc/concept/audio-jitter.md` (normative), with test
vectors beside the [conformance corpus](/doc/concept/audio-jitter.md):
input PCM, buffered distance, target, and hysteresis state, the expected accelerate, expand, or hold, and the expected output. The
second one conforms to them. Neither waits on the other, since the design
is NetEq's, not either implementation's.

- Stretch on the PCM the ring hands to the device callback, driven by the
  distance between buffered audio and the target. The callback stays
  allocation-free and lock-free, as the ring is today.
- Padding with silence remains only for a ring the stretch couldn't keep fed,
  and a trim only for a distance it can't close within a bound.
- Verification: the native rows reach 0 `underruns` per minute after the
  target settles, and their provisional budgets are re-measured on the nightly
  runner by the budgets note's rule. A listening check confirms that a 2 ms/s
  drift and the measured expansions are inaudible or close to it.

Public API: none expected. Wire: none.

## Related

- [Time stretch](/quest/m2/watch-audio-time-stretch.md) - the browser stretch sharing this algorithm and its vectors, which #7679 folds into Watch
- [Watch](/quest/m1/audio-jitter-target/watch.md) - the browser's measured target, and the browser stretch's home once #7679 lands
