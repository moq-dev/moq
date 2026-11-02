# [S] One support probe for watch audio and video

## Goal

The watch audio and video `Source`s probe rendition support through one
private helper that owns the race against an unsettled probe, the config-keyed
cache, `pending`, and abort. Audio gains video's cache and an `out.error` that
is `NotSupportedError` when no audio rendition is supported, and the bundled
UI's unsupported indicator reports an audio-only broadcast it cannot play.
Rendition selection stays separate per kind. Additive `@moq/watch` API; no
wire impact.

## Plan

Decided 2026-10-27 (maintainer interview, from the 2026-10-27 refactor audit):

- Audio exposes `out.error` like video, and the unsupported indicator is its
  consumer: it fires when video is unsupported, or when the catalog has no
  video and audio is unsupported.
- Video's `#runSupported` (`js/watch/src/video/source.ts`) caches per
  rendition by `supportCacheKey` or the JSON config and sets the error; audio's
  (`js/watch/src/audio/source.ts`) re-probes every run and only logs. The
  helper takes video's behavior for both.
- Test a catalog replaced while a probe is pending, a probe that never
  settles, a rejecting probe, and a catalog change that leaves configs
  unchanged (served from cache).
