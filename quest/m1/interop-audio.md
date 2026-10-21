# [L] Every interop subscriber decodes the audio tone

## Goal

`just test interop --all` proves audio delivery the way it proves video: every
subscriber decodes the audio track and checks the publisher's 440 Hz tone, and
every publisher sends one. A garbled decode (wrong channel order, pre-skip, or
sample rate), a missing audio rendition, or a timestamp gap fails the matrix.

Today only the browser subscriber looks at audio, and only for
`audioBytes > 0`. The Rust, Python, Go, C, C++, native Node/Bun, and GStreamer
subscribers all select the video track, and the Rust CLI publisher sends no
audio, so native audio delivery is untested end to end.

## Plan

Decided in planning (2026-10-20):

- Depth: decode and verify the tone, not just count frames. A Goertzel filter
  at 440 Hz and timestamp continuity per subscriber; bytes arriving is the bar
  that already lets a broken decode pass. A multichannel publisher sends a
  distinct frequency per channel, so a channel swap fails too.
- The Rust CLI publisher adds an Opus track in fMP4 (ffmpeg `sine` at
  440 Hz), which also covers moq-mux's Opus import. AAC was the first choice,
  but the Node and Bun subscribers fall back to an Opus-only libav build, so
  an AAC track waits on [AAC in the native JS
  fallback](/quest/m1/libav-aac.md), which adds it once that lands.
- Folded in from the same gap analysis: the `just test wire-compat` media lanes
  gain an audio check (released vs current, decoded), and moq-transcode gains
  a test that an audio rendition passes through untouched.

Recommendations:

- Decode with what each client already has: moq-ffi's audio decode for Python,
  Go, C, and C++, moq-audio for the Rust subscriber, `decodebin` on `audio_0`
  for GStreamer, WebCodecs for native JS. A subscriber whose binding cannot
  decode audio is a binding gap to report, not a reason to skip the check.
- The browser subscriber upgrades from `audioBytes > 0` to the same tone check;
  the media QA probe (`test/interop/clients/js/src/probe.ts`) already detects
  tones.
- Add a negative control like media QA's silent-audio and audio-offset faults:
  a publisher sending silence or the wrong frequency must fail every
  subscriber.
- Keep the expected tone in one place the clients share, so a future publisher
  change does not desync the checks.

Public API: none. Wire: none.

## Related

- [AAC in the native JS fallback](/quest/m1/libav-aac.md) - lets the Rust publisher add an AAC track every subscriber can decode

- [Bindings in the interop matrix](/quest/m2/interop-bindings.md) - Kotlin, Swift, and Dart clients; whichever lands second carries the audio check to them
- [Native audio decode conformance](/quest/m1/audio-decode-conformance.md) - the same decode paths, checked in unit tests
- [Media QA audio cases](/quest/m2/media-qa-audio.md) - grades A/V sync and codec changes in the browser
