# [S] Audio publish mute and pause are clean

## Goal

Two `@moq/publish` audio fixes. Volume 0 silences the microphone within a
configurable ramp, 50 ms by default, instead of a fixed 200 ms. Disabling
the encoder with a subscriber attached ends the track's epoch, as video
already does.

## Plan

Decided (2026-10-04):

- Producer (#4781): JS audio hand-writes its discontinuity marker and its
  one-group-per-frame writes, and writes the marker only when demand
  disappears. Move audio onto `Container.Legacy.Producer`, as video uses, and
  call `discontinuity()` from the pipeline cleanup, so any pipeline stop
  (disable, format change, fatal error, demand loss) ends the epoch. This
  deletes the hand-written marker code; `<moq-publish muted>` drives
  `enabled`, so it is the stock mute path.
- Fade (#4782): `FADE` in `js/publish/src/audio/gain.ts` is a fixed 0.2 s for
  a full swing. Audio.Encoder props gain `fade?: Time.Milli |
  Signal<Time.Milli>` beside `volume`, typed like `frameDuration`, default
  50 ms. Every volume change ramps over `fade` whatever its size, so a mute
  always completes in `fade`. 0 is an instant step; a negative or NaN value
  throws. The watch emitter's own `FADE_TIME` of 0.2 s gets the same option.
  Document both.

Tests: volume 0 is silent after `fade`; `fade` 0 steps at once; disabling
with a subscriber attached writes a marker.

## Closes

- [#4781](https://github.com/moq-dev/moq/issues/4781) - close this issue when the quest finishes
- [#4782](https://github.com/moq-dev/moq/issues/4782) - close this issue when the quest finishes

## Related

- [Audio group duration](/quest/m1/audio-group-duration.md) - builds on the shared producer
- [LIFO cleanups](/quest/m1/signals-lifo.md) - the teardown error from the same capture
