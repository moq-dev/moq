# [L] A browser voice source stops sending soon after background noise rises

## Goal

`@moq/publish`'s voice gate shuts within a few seconds when the background
under a silent talker gets louder mid-call, such as a fan switched on or the
browser's gain control raising the room tone after a quiet talker, as libopus
does at the complexity both publishers encode with. Speech is still never
dropped, and players change nothing.

## Plan

The gate's VAD is a bit-exact port of SILK's (`silk/VAD.c`), whose per-band
noise floor adapts fast only for its first 20 s and slowly after. Measured in
[moq-dev/moq#8998](https://github.com/moq-dev/moq/pull/8998): a fan switched
on at -51 dBFS 20 s into a call reads as speech for about 12 s, then as
scattered speech in about 16% of blocks, so the 200 ms hangover keeps the gate
mostly open and the fan saves 10% of its bytes, against 99% for a fan running
from the start. Chrome's gain control lifting room tone from about -60 to -52
dBFS after a quiet talker does the same (2 to 27% saved). libopus at
complexity 7 and up (WebCodecs' desktop default is 9, the Rust publisher's 10)
lets its tonality analysis (`src/analysis.c`, a small MLP over spectral
features) override SILK's decision, and suppresses both.

Recommended: port the analysis's activity probability, matched against
libopus on the same inputs, so the browser gate decides as libopus does at the
complexity the Rust publisher's DTX gate runs.

## Waiting

- Maintainer: pick the detector. (a) Port libopus's tonality analysis activity
  (recommended: it is what libopus itself does at complexity 7+, so both
  publishers suppress alike; about 1,100 lines of C plus its weights and an FFT, checked
  against libopus as the SILK port was). (b) Speed up the SILK floor's rise
  after 20 s (small, but the gate stops matching any libopus and must be tuned
  against speech loss by hand). (c) Leave it: room tone is already 96 to 98%
  suppressed and rising noise only costs bytes, never speech.

## Related

- [Rust and JS share an optional audio kind](/quest/m2/audio-kind.md) - the Rust publisher's DTX gate, whose libopus already runs the analysis
