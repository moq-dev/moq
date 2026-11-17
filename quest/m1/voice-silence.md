# [M] A browser voice source sends nothing while its talker is silent

## Goal

`@moq/publish` suppresses silence on an open mic: for a source of
`kind: "voice"` (the microphone source's default), a voice activity detector
drives the gate push-to-talk's mute already uses, so a silent talker sends
nothing and players fill the span with comfort noise, as they do for a mute.
It works in every browser, changes no public API, and sets no WebCodecs
`usedtx`. Music and kindless sources are not gated.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan), after
[chromium#581458361](https://issues.chromium.org/issues/581458361) was fixed in
Chrome 160 ([cf1522282e](https://chromium.googlesource.com/chromium/src.git/+/cf1522282ec2ae29179405d6d94fa94e3df3550f),
stable 2027-01-05):

- **Open-mic silence suppression is wanted now ✅** (rejected: delete the
  tracking quest and do nothing; track the fix until stable; file a second
  Chromium bug for input jumps, which our encoder never relies on). This
  reverses the 2026-11-04 "no VAD until open-mic suppression has a consumer".
- **Voice gets it by default, no knob ✅** (rejected: an opt-in `OpusConfig`
  field; default plus an override). Neither has a caller.
- **One wire shape for every publisher, push-to-talk's ✅** (rejected: native
  `usedtx` with a declared end once a keep-alive shows suppression; sending
  keep-alives as-is and teaching every player to read Opus's VAD bit).
  WebCodecs emits nothing for a suppressed frame, so the publisher learns
  silence began only at the next keep-alive about 400 ms later, after the
  player has run dry; keep-alives read as active audio; and `usedtx` works
  only on Chrome 160+ and Firefox (Safari ignores it). Our gate declares the
  end the moment silence starts. On the wire, silence costs nothing, against
  about 2.9 packets/s and 1.3 kbps per subscriber for DTX keep-alives
  (measured in [moq-dev/moq#7880](https://github.com/moq-dev/moq/pull/7880));
  each pause costs one marker group and a new group on resume, and the
  listener resets its decoder once per pause. Players change nothing.
- **The VAD is a JS port of SILK's (libopus `silk/VAD.c` and its filterbank,
  BSD) ✅** (rejected: an adaptive energy VAD; #7880's fixed -45 dBFS energy
  gate; libopus's VAD bit read from encoded chunks; a WASM or neural VAD). It
  is the detector libopus itself uses for voice in SILK mode, so this and the
  Rust publisher suppress alike, and its per-band adaptive noise floor gates
  steady background noise such as a fan.

Recommendations for the implementer: libopus waits 10 inactive frames (200 ms)
before suppressing; start there, with one frame of pre-roll fed before speech
resumes as #7880 did (no speech frame lost, head within 5 ms of unsuppressed).
Silence is dropped before the encoder, as a mute is, so it costs no encode.
Mute and the VAD feed one gate; mute wins.

Validate on real capture in Chrome and Firefox over a speech, pause, fan-noise,
and quiet-talker recording: bytes sent during silence, clipped word heads or
tails, a listening check of the comfort noise, and the VAD's decisions against
libopus's own on the same input.

## Related

- [Rust and JS share an optional audio kind](/quest/m2/audio-kind.md) - the Rust publisher's gate, on the same wire shape
