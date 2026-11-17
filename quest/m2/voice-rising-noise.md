# [L] A browser voice source stops sending soon after background noise rises

## Goal

`@moq/publish`'s voice gate shuts within a few seconds over steady background
noise under a silent talker, including noise that starts or rises mid-call,
such as a fan switched on or the browser's gain control lifting the room tone
after a quiet talker, as libopus does at the complexity both publishers encode
with. It clips no more speech than today, and players change nothing.

## Plan

The gate's VAD is a bit-exact port of SILK's (`silk/VAD.c`), whose per-band
noise floor adapts fast only for its first 20 s and slowly after. Measured in
[moq-dev/moq#8998](https://github.com/moq-dev/moq/pull/8998) on real browser
capture: a fan switched on 20 s into a call reads as speech for about 12 s,
then as scattered speech, so the 200 ms hangover keeps the gate mostly open
and the fan saves 3 to 44% of its bytes. Gain control lifting room tone from
about -60 to -52 dBFS after a quiet talker saves 2 to 28%. With the browser's
processing off, even a fan running from the start saves only 24 to 34%; with
it on, noise suppression removes that fan before the gate sees it. libopus at
complexity 7 and up (WebCodecs' desktop default is 9, the Rust publisher's 10)
lets its tonality analysis (`src/analysis.c`, a small MLP over spectral
features) override SILK's decision, and on the same input DTXes the fan that
SILK alone keeps sending.

Recommended: port the analysis's activity probability, matched against
libopus on the same inputs, so the browser gate decides as libopus does at the
complexity the Rust publisher's DTX gate runs.

## Waiting

- Maintainer: pick the detector. (a) Port libopus's tonality analysis activity
  (recommended: it is what libopus itself does at complexity 7+, so both
  publishers suppress alike; about 1,100 lines of C plus its weights and an FFT, checked
  against libopus as the SILK port was). (b) Speed up the SILK floor's rise
  after 20 s (small, but the gate stops matching any libopus and must be tuned
  against speech loss by hand). (c) Leave it: a quiet room is already 92 to 100%
  suppressed and rising noise only costs bytes, never speech.

## Related

- [Rust and JS share an optional audio kind](/quest/m2/audio-kind.md) - the Rust publisher's DTX gate, whose libopus already runs the analysis
