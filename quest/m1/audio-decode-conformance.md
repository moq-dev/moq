# [L] Native audio decode passes a conformance suite like moq-video's

## Goal

Every native audio decode path is checked against a known input, the way
`rs/moq-video/src/decode/conformance.rs` checks video against reference YUV.
Opus and AAC decoded through each moq-audio backend come out at the right
frequency, level, channel order, sample count, and timestamps, and the
backends agree. The C and FFI audio decode surfaces are exercised by tests,
not only by applications.

## Plan

The implementation is checkpointed in [moq-dev/moq#6261](https://github.com/moq-dev/moq/pull/6261).
Finish validation before retiring this quest:

- Run the final partition/drain/reset/overflow properties and default/no-default
  audio test suites through `quest gate`. The prior native suite passed all 171
  tests after the duration and fractional-phase timestamp fixes; the expanded
  property and final fixture-loader changes still need a fresh run.
- Run `just ci`, `just test interop --all`, and `just rs bench-playback` through
  the gate. Host load prevented admission at checkpoint time. Do not bypass it.
- Re-run the C ABI and installed C++ consumers on the final source. The earlier
  external C fixture, C++ build/consumer matrix, and targeted FFI audio tests
  passed before the final resampler changes.
- Check reference regeneration against the committed inputs using the documented
  FFmpeg version and source hash, then address any substantive review findings.
- Delete this quest and every link once those checks pass.

The suite uses stepped stereo tones, NASA public-domain speech in AAC/Opus
CMAF, and family-one 5.1 Opus through the supported MPEG-TS importer. The existing
`bbb.mp4` contains silence followed by an invalid AAC sample, so its consumers
remain unchanged and a licensed recording supplies the real reference instead.
`test_data/README.md` records provenance, commands, measured error bounds,
pre-skip/warmup alignment and why AAC PNS noise is not compared across reset.

No public API or wire changes. Keep the conformance hooks private and preserve
source timestamps across jumps while repaying the exact rounded stream duration.

## Related

- [Wire-compat audio](/quest/m1/wire-compat-audio.md) - the same decoders, end to end across releases
- [AudioToolbox decode](/quest/m2/audio-decode-audiotoolbox.md) - a new backend that should join this suite when it lands
- [Opus backends](/quest/m2/audio-opus-backend.md) - compares Opus implementations; this suite is its correctness floor
