# [S] Portrait transcode ladders

## Goal

Portrait (vertical) video plays correctly end to end through `moq-transcode`,
and transcode rungs are sized by the source's short side, so a 1080x1920
source gets 720x1280 and 360x640 rungs rather than rungs a third as wide.

## Plan

`rs/moq-transcode` defines rungs by height (`rs/moq-transcode/src/ladder.rs`)
and derives width from the source aspect ratio (`resolve_rungs` in
`rs/moq-transcode/src/catalog.rs`). Its own test pins a 1080x1920 source's
360 rung at 202x360, so a portrait source gets landscape-sized pixel counts at
landscape bitrates. Verify the gap end to end before changing it.

- Treat a rung's configured size as the short side, and apply the never
  upscale and below-source checks to the short side too.
- A source coded landscape with a catalog `rotation` keeps its rotation on
  every rung, as it does today; check that the short side is taken from the
  coded frame so both portrait encodings land on the same rungs.
- Prove one portrait source through publish, transcode, and `<moq-watch>`,
  plus unit tests for both portrait encodings.

## Related

- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - portrait phones publishing a catalog rotation
- [iOS capture](/quest/m1/mobile-capture-ios.md) - native portrait capture
- [Android capture](/quest/m1/mobile-capture-android.md) - native portrait capture
