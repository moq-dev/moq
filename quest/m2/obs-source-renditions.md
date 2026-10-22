# [S] OBS source picks renditions by the catalog contract

## Goal

The OBS MoQ source subscribes to the best enabled rendition it can decode,
in the same order hang's `ranked()` uses. It resubscribes when a referenced
rendition's publisher epoch changes. It never waits on a disabled rendition
while an enabled one is available.

## Plan

P2 correctness, reproduced against the production functions in the
2026-10-22 binding and OBS audit at main
[1f5caa7026](https://github.com/moq-dev/moq/commit/1f5caa7026). Both defects
have existed since the OBS plugin moved onto the C++ package in
[moq-dev/moq#4079](https://github.com/moq-dev/moq/pull/4079).

- Selection: `moq_source_first_rendition` in `cpp/obs/src/moq-source.cpp`
  picks the rendition whose name sorts first. It ignores `enabled`,
  `preference`, picture size, and bitrate, and never falls through to a
  decodable alternative. With `a` disabled and `b` enabled, it picks `a`.
  The audio consumer then rejects that choice, and the video consumer waits
  for frames that never come.
- Epoch: `moq_source_same_video` and `moq_source_same_audio` leave
  `epoch` out of the comparison. A stable parent catalog that points to a
  restarted sibling publisher keeps the old subscription. The connection's
  `instance` counter only covers the parent's own restarts.

Decided with the maintainer on 2026-10-22:

- Mirror hang's `ranked()` per section in OBS, with ties in name order.
  Video (`hang::catalog::Video::ranked`) sorts by enabled, then preference,
  then coded area, then bitrate. Audio (`hang::catalog::Audio::ranked`) sorts
  by enabled, then bitrate, then sample rate, then channel count. Skip
  disabled renditions and take the first one the source can decode. A
  section where every rendition is disabled is handled like an absent
  section: clear the track, and blank it for video. Don't add a ranking API
  to moq-ffi until another binding consumer needs one.
- Make `epoch` part of rendition identity for both the active and the
  pending track.

Extend `cpp/obs/test/moq-source-test.cpp` with these cases: video and audio ranking order, a
disabled first rendition, an all-disabled section, and an epoch-only change.
No public API or wire change.

## Related

- [OBS video source replacement](/quest/m2/obs-moq-video/source.md) - replaces the decode path; selection and identity stay in this file either way
