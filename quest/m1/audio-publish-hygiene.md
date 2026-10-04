# [S] Audio publish teardown, mute, and pause are clean

## Goal

Three `@moq/publish` audio fixes. Capture teardown logs nothing. Volume 0
silences the microphone within a click-free ramp instead of 200 ms. Disabling
the encoder with a subscriber attached ends the track's epoch, as video
already does.

## Plan

Decided (2026-10-04), one quest because each is a few lines in the same
package:

- Teardown (#4788): `js/publish/src/audio/capture.ts` registers
  `root.disconnect()` before the nested `root.disconnect(worklet)`, so the
  second throws `InvalidAccessError` and signals logs "cleanup error". Delete
  the outer disconnect; `context.close()` and the nested cleanup cover it.
  Make the test fake's `disconnect(node)` throw for an unconnected node, as
  browsers do.
- Mute ramp (#4782): `FADE` in `js/publish/src/audio/gain.ts` is 0.2 s.
  Shorten it to a click-free ramp (about 10 to 20 ms) and document it on
  `volume`. A better constant, not a new option.
- Pause marker (#4781): `js/publish/src/audio/encoder.ts` writes the
  discontinuity marker only when demand disappears. Gate the marker effect on
  the same inputs as the pipeline (enabled, format, fatal, track), so any
  pipeline stop ends the epoch, mirroring `js/publish/src/video/encoder.ts`.

Tests: teardown with the strict fake logs nothing; volume 0 is silent within
the ramp; disabling with a subscriber attached writes a marker.

## Closes

- [#4781](https://github.com/moq-dev/moq/issues/4781) - close this issue when the quest finishes
- [#4782](https://github.com/moq-dev/moq/issues/4782) - close this issue when the quest finishes
- [#4788](https://github.com/moq-dev/moq/issues/4788) - close this issue when the quest finishes
