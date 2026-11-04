# [XS] Chromium's Opus encoder stamps chunks from their input

## Goal

Chromium's WebCodecs `AudioEncoder` stamps each Opus chunk with the timestamp
of the input audio it encodes, both across `usedtx` silence and across a jump
in input timestamps. Check by running the repro attached to
[chromium#581458361](https://issues.chromium.org/issues/581458361) in a released
Chrome: scenario A ends near 13.98 s with steps across
suppressed spans, and scenario C follows the +1 s jump. Delete this quest once a stable Chrome
passes. Nothing of ours waits on it: the publisher gates silence before the
encoder and stamps chunks from its own inputs ([Push-to-talk suppresses
silence](/quest/m1/push-to-talk/dtx.md)), and Safari ignores `usedtx`.

## Plan

Measured 2026-11-04: Chrome 156 ends 14 s of DTX input
at 8.92 s and ignores input timestamp jumps. In
`media/audio/audio_opus_encoder.cc`, the timestamp tracker is seeded once from
the first input and advances only when a packet is emitted, so DTX's
non-transmitted frames and input gaps are both lost. WebCodecs §5.6 already
requires the input's timestamp, so this is a Chromium bug; a spec issue
([w3c/webcodecs#986](https://github.com/w3c/webcodecs/issues/986)) was opened
and closed by the maintainer for that reason.

Decided 2026-11-04 (maintainer): file the Chromium bug
([chromium#581458361](https://issues.chromium.org/issues/581458361), Blink>Media>WebCodecs,
submitted by the maintainer) and no spec issue; track it here in m2, since
push-to-talk works around it by gating silence before the encoder and stamping
chunks from its own inputs, so nothing depends on this.

## Waiting

- Chromium: fix [chromium#581458361](https://issues.chromium.org/issues/581458361)
  so Opus chunks carry their input's timestamp across `usedtx` silence and
  input jumps. Check by running the attached repro in the latest stable Chrome
  (scenario A ends near 13.98 s, scenario C follows the +1 s jump); advance it
  by commenting on the bug with each stable release that still fails.
