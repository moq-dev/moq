# [S] Set catalog rotation from the live camera's orientation

## Goal

A phone held in portrait publishes a catalog `rotation`, so `<moq-watch>`
shows the picture upright instead of the viewer guessing from the aspect
ratio. Repro: `<moq-publish>` on an iPhone in portrait, watched from a
desktop; the catalog says 640x480 with no `rotation` and the picture is
sideways.

## Plan

The rest of #933 is done: the renderer applies `catalog.video.rotation`
(`@moq/video`, `js/video/src/presentation.ts`) and file import rotates stored footage
(`rotator` in `js/publish/src/source/file.ts`). Live camera capture never sets
the field.

- Add `rotation: Getter<number>` beside the `flip` input in
  `js/publish/src/broadcast.ts` and write `section.rotation` next to
  `section.flip`.
- Detect orientation in `js/publish/src/source/camera.ts` (the track's
  settings, `screen.orientation`, or `VideoFrame.rotation`) and re-publish on
  change.
- Wire the signal through `<moq-publish>` beside `#flip`
  (`js/publish/src/element.ts`).
- Pass it to the `<canvas>` preview's `presentation` beside `flip`
  (`js/publish/src/preview.ts`), swapping the display size for a quarter turn.
  The preview already draws through the shared rotation code; decided in #5138
  to wait for this setting rather than add a preview-only rotation.
- `demo/web` sets neither `flip` nor `rotation`, so nothing changes there.

Additive on `@moq/publish`, so it lands on main.

## Closes

- [#933](https://github.com/moq-dev/moq/issues/933) - close this issue when the quest finishes
