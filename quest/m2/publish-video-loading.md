# [XS] A loading video preview takes no space

## Goal

A `<video>` preview in `<moq-publish>` stays hidden until it reports its media
size, so `<moq-publish-ui>` never retains the browser's pre-metadata
placeholder ratio (300x150) as the empty player's aspect ratio. Hiding,
showing, and quickly hiding the camera again leaves the empty player at the
last real preview's ratio.

Non-goals: the `<canvas>` preview and the UI's size observer, which
[moq-publish hides previews that show nothing](/quest/m2/publish-empty-previews.md)
owns.

## Plan

Decided 2026-11-03 (user, /quest-import of
[moq-dev/moq#7826](https://github.com/moq-dev/moq/issues/7826)):

- Goal ✅ Fix it, rather than won't fix (cosmetic and self-healing) or asking
  [moq-dev/moq#7832](https://github.com/moq-dev/moq/pull/7832) to absorb it,
  since that PR scoped it out.
- Approach ✅ Hide until sized: in `js/publish/src/element.ts`'s preview
  effect (the `<video>` branch, `preview.style.display = "block"` right after
  setting `srcObject`, `:362` as of 57902187f6), keep the video
  `display: none` until `videoWidth > 0` (`loadedmetadata` or `resize`), then
  show it. This follows the required quest's rule that a preview showing
  nothing is hidden, and leaves `js/publish/src/ui/element.ts` sizing purely
  from the rendered size. Rejected: the UI ignoring a slotted video with no
  `videoWidth` (couples the UI to the preview element type), and pre-sizing the
  video from `track.getSettings()` (settings can be missing or pre-rotation,
  so it needs this fallback anyway).
- Milestone ✅ m2, after the required quest, since both edit the same preview
  effect and the race is cosmetic.

Confirm a `display: none` video with a `MediaStream` `srcObject` still reaches
`HAVE_METADATA` in Chromium; if a browser withholds it, hide it without
removing it from layout (such as `visibility: hidden` with zero size) instead.
Remove the listener in the effect's cleanup so a stale stream can't show the
element.

Test in `test/interop/clients/js/publish-ui.browser.ts` beside the required
quest's hide/show cases: restore the preview, set `invisible` on the next
`loadedmetadata` (the report's targeted sequence, 10/10 on unmodified code),
and assert the empty player keeps the previously observed height.

Public API: none. Wire: none.

## Required

- [moq-publish hides previews that show nothing](/quest/m2/publish-empty-previews.md) - `<moq-publish>` hides a preview that renders nothing

## Closes

- [#7826](https://github.com/moq-dev/moq/issues/7826) - a quick hide during video restore retains the loading placeholder's aspect ratio
