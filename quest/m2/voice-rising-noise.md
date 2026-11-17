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

Decided 2026-11-17 (maintainer, on
[moq-dev/moq#8998](https://github.com/moq-dev/moq/pull/8998#issuecomment-6947837677)):
that PR ships with this gap, and this quest ✅ ports libopus's analysis
activity probability (`src/analysis.c`, about 1,100 lines of C plus its MLP
weights and an FFT), so the browser gate decides as libopus does at the
complexity the Rust publisher's DTX gate
(`rs/moq-audio/src/encode/backend/libopus.rs`) runs, and both publishers suppress
noise that starts mid-call alike. Check it against libopus on the same inputs,
as the SILK port was. Rejected: (b) speeding up the SILK floor's rise after
20 s, which matches no libopus and must be tuned against speech loss by hand;
(c) leaving it.
