# [L] One enabled flag says whether to use a rendition

## Goal

A hang rendition carries `enabled`, default true. `enabled: false` means no
frames are coming, and a consumer MUST NOT select it. It replaces both the
encoder-lag `stalled` flag and a separate pause flag: whoever decides a
rendition should not be used (an app pausing it, or the bandwidth allocator)
writes the same field, and a publisher mute is a one-field catalog delta
instead of a removal.

## Plan

Decided (2026-10-04):

- Wire: an optional `enabled` boolean on every rendition, audio and video,
  written only when false. Writers stop writing `stalled`, and readers ignore
  it: released publishers still flap it, and following it would keep #4772
  alive for them. Update
  `drafts/draft-lcurley-moq-hang.md` (replacing the `stalled` section, with a
  changelog entry, since `stalled` shipped in -03), `doc/concept/hang.md`, and
  run `just drafts check`. MSF stops mirroring `stalled` and does not mirror
  `enabled`.
- Delete the encoder-lag detector: `Stalled.Detector` in
  `js/hang/src/catalog/stalled.ts` and `rs/hang/src/catalog/stalled.rs`, its
  producers in `js/publish/src/video/encoder.ts` and
  `rs/moq-mux/src/codec/video.rs`, and its importer hooks. It is what flaps
  (#4772, #4776): frame-count hysteresis, plus clearing on demand loss. No
  replacement adapts an overloaded encoder; that waits for a consumer.
- Publisher pause: `enabled` false on `@moq/publish` keeps the rendition in
  the catalog with `enabled: false` instead of removing it, and the encoder
  stops. The encoder keeps the last config it published, since muting also
  releases the capture. Before disabling a video rendition it encodes one
  black keyframe, so a viewer released before this field shows black rather
  than a frozen picture. Older viewers otherwise keep selecting a disabled
  rendition; that degradation is accepted and noted in the changelog.
- Viewer: `@moq/watch` deselects a disabled rendition, and its audio graph
  outlives the absence as it does for a removed one.
- Bandwidth: a rendition is enabled only once its reservation is granted, and
  disabled, with encoding stopped, when the grant falls below its floor,
  until the grant recovers. A disabled rendition loses its subscribers, and
  the allocator grants nothing to an undemanded track, so recovery evaluates
  a hypothetical share against the current estimate instead of waiting for a
  grant. The [ladder](/quest/m3/ladder/README.md) and
  [audio grant following](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md)
  adopt that rule; this quest only defines the field.
- Everything else that names `stalled` moves to `enabled`: moq-transcode's
  `inherit_stalled`, the moq-ffi field and its wrappers, and the C
  `moq_consume_video_stalled`, which becomes `moq_consume_video_enabled`.
  Selection in `js/watch/src/video/source.ts` filters on it, as
  [rendition preference](/quest/m1/rendition-preference.md) expects.

Tests: disabling an audio rendition publishes a catalog with the same
rendition and `enabled: false`, and a viewer deselects it and keeps one
AudioContext across disable and enable; a legacy `stalled: true` changes
nothing; nothing in the tree writes `stalled`.

## Closes

- [#4772](https://github.com/moq-dev/moq/issues/4772) - close this issue when the quest finishes
- [#4776](https://github.com/moq-dev/moq/issues/4776) - close this issue when the quest finishes

## Related

- [Ladder](/quest/m3/ladder/README.md) - disables a rung its grant cannot sustain
- [Rendition preference](/quest/m1/rendition-preference.md) - the other per-rendition selection field
