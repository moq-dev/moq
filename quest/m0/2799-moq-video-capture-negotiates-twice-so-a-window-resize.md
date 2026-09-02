# [M] moq-transcode: the ladder follows a source resolution change

## Goal

A transcoded broadcast whose source changes resolution mid-stream serves a
ladder sized for the picture it now carries. A window resized between the
capture probe and the first subscriber, a reconnecting publisher, or a
renegotiated screen share all end up with rungs that fit.

## Plan

`moq_video::encode::publish_capture` opens its source twice by design: once at
startup to probe the mode and advertise an exact rendition, and again when the
first subscriber arrives. Camera modes are stable across the two opens; macOS
window capture is not, since `screencapture.rs` derives geometry from the
window on every open. The catalog recovers on its own once frames flow, because
the importer republishes the SPS dimensions. What does not recover is
`moq_transcode::run`, which calls `resolve_rungs` once and says so: the rung
set is fixed at startup, only the passthrough entries track the source.

Holding the source open from the probe to the first subscriber would close the
capture case only, at the cost of the idle camera the demand gate exists to
release. Letting the ladder follow the source closes every case, so that is
the fix, and the two opens stay.

- On a catalog snapshot whose chosen source rendition changed dimensions,
  re-run `resolve_rungs` and diff against the live set: retire rungs that no
  longer fit (finish their tracks, drop their catalog entries), probe and add
  the new ones, keep the rest. A browser subscriber on a retired rung sees
  its track end and picks another rendition, the same as any rendition change.
- `moq play` does not: `rs/moq-cli/src/play.rs` stops following the catalog
  once video and audio have both started, so a retired rung ends playback or
  leaves audio alone. Re-arm its catalog loop when a started track ends, so
  native playback reselects too; that is part of this quest, not a follow-up.
- Test: a source that republishes its rendition at a new size; assert the
  ladder changes, passthrough follows, and a subscriber on a retired rung is
  ended cleanly. For `moq play`, complete the active video task and assert a
  later catalog snapshot starts the replacement track, so an implementation
  that leaves `video_started` set fails.

## Closes

- [#2799](https://github.com/moq-dev/moq/issues/2799) - close this issue when the quest finishes

## Related

- [Window capture lifecycle](/quest/m0/capture-window-lifecycle.md) - the other half of the resize story, on the live capture path
