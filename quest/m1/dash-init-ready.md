# [S] A DASH manifest never waits on one rendition's init

## Goal

Rendering a DASH manifest never blocks on a rendition whose init segment is
not known yet. It lists every enabled rendition whose init exists and leaves
out the rest until a later refresh. Today `build_inits` in
`rs/moq-hls/src/export/mod.rs` awaits each rendition's `latest_init` serially,
disabled ones included, after the readiness wait and with no bound of its own,
so an inline-codec rendition bootstrapping from a still-open group holds up
the whole manifest despite healthy alternatives.

## Plan

From the 2026-10-24 moq-hls audit. Decided with the maintainer on 2026-10-24:
report current state, rather than build inits concurrently under the existing
readiness wait, which still lets one stuck rendition hold the manifest.

- Consider enabled renditions only, and gather their inits concurrently
  without letting any one of them gate the render.
- A rendition whose init is not built yet is left out of this MPD (the render
  already drops a rendition with no init) and appears once its init exists.
- The `/init.*.mp4` route has the same unbounded wait; it answers only once
  the init exists.
- Tests with mocked time: one rendition stuck on an open group, the manifest
  renders its siblings immediately, and the stuck one appears after its group
  closes; a disabled rendition's init is never built.

Public API: none. Wire: none.
