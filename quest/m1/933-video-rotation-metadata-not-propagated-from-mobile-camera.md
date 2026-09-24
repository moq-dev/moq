# [S] Set catalog rotation from the live camera's orientation

## Goal

A phone held in portrait publishes a catalog `rotation`, so `<moq-watch>`
shows the picture upright instead of the viewer guessing from the aspect
ratio. Repro: `<moq-publish>` on an iPhone in portrait, watched from a
desktop; the catalog says 640x480 with no `rotation` and the picture is
sideways.

## Plan

The rest of #933 is done: the renderer applies `catalog.video.rotation`
(`js/watch/src/video/renderer.ts:187-198`) and file import rotates stored
footage (`js/publish/src/source/file.ts:283`, `:303-305`). Live camera capture
never sets the field.

- Add `rotation: Getter<number>` beside `flip` in the broadcast inputs
  (`js/publish/src/broadcast.ts:25-26`) and write `section.rotation` next to
  `section.flip` (`:178`).
- Detect orientation in `js/publish/src/source/camera.ts` (the track's
  settings, `screen.orientation`, or `VideoFrame.rotation`) and re-publish on
  change.
- Wire the signal through `<moq-publish>` beside `#flip`
  (`js/publish/src/element.ts:121-122`).
- `demo/web` sets neither `flip` nor `rotation`, so nothing changes there.

Additive on `@moq/publish`, so it lands on main.

## Closes

- [#933](https://github.com/moq-dev/moq/issues/933) - close this issue when the quest finishes
