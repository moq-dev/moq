# [M] Native media's small surface fixes

## Goal

- `publish_capture` returns a handle plus a driver in both moq-audio and
  moq-video, mirroring each other, and moq-audio's `encode::{Control, State,
  Status}` stop being separate public names. The handle may start with only
  the state accessors audio has today, so
  [video-publish-grant](/quest/m3/video-publish-grant.md) adds `grant()`
  without a break (added 2026-11-07, maintainer, /quest-plan from the 2026-11-07 API sweep of open PRs and remaining quests).
- `on_demand` means one thing across crates. moq-video's closes the camera
  between viewers and defaults to `false`; moq-audio's keeps the device open,
  defers encoding, and replays retained PCM, and defaults to `true`. Audio's
  gets a name for its role (for example `retain`), and `@moq/publish`'s
  audio `capture` mode is checked for the same name.
- moq-video's encode producer folds `with_track` into `new`.
- `encode::Applied` reports `cut`, so callers stop matching
  `CutUnsupported` to learn it.
- moq-audio's playback `Sink::{set_volume, volume, peak}` go, since
  `Control` has them, and the audio decode `Consumer::max_delay()` duplicate
  goes.
- The render `Exporter::new` returns the settled `encode::Config` instead
  of rewriting the caller's `&mut` config.

## Plan

Found by the 2026-11-07 API shape audit (native media); see the
[epic](/quest/m2/api-shape/README.md). moq-ffi, moq-c, and the wrappers
follow each rename. Check downstream audio
callers of `on_demand` before picking the new name.
