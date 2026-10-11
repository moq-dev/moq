# [S] Portrait transcode ladders

## Goal

Portrait (vertical) video plays correctly end to end through `moq-transcode`,
and transcode rungs are sized by the source's short side, so a 1080x1920
source gets 720x1280 and 360x640 rungs rather than rungs a third as wide.

## Plan

The draft uses coded short-side pixels in `rs/moq-transcode/src/ladder.rs`
and `resolve_rungs` in `rs/moq-transcode/src/catalog.rs`. The original
1080x1920 source produced 202x360 for a 360 rung; the implementation now
produces 360x640 and preserves rotation.

- Treat a rung's configured size as the short side, and apply the never
  upscale and below-source checks to the short side too.
- A source coded landscape with a catalog `rotation` keeps its rotation on
  every rung, as it does today; check that the short side is taken from the
  coded frame so both portrait encodings land on the same rungs.
- Prove one portrait source through publish, transcode, and `<moq-watch>`,
  plus unit tests for both portrait encodings.

### Progress

Geometry regressions and the full package checks pass. All-language interop
passes. The browser paints the selected 360x640 rung through software
publish/transcode/decode, and the harness now has its own anonymous local
relay fixture instead of depending on the interop authentication server.

End-to-end validation remains blocked: closing a superseded catalog subscription
raises an uncaught `subscribe stream ended without SUBSCRIBE_END` error in the
unchanged JS network code. The portrait harness now rejects collected browser
errors instead of silently passing them. Resolve the local cancellation and
promise ownership path, then complete the strict portrait run before closing
this quest. No timeout or retry is used to hide that failure.

## Related

- [#933](/quest/m1/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - portrait phones publishing a catalog rotation
- [iOS capture](/quest/m1/mobile-capture-ios.md) - native portrait capture
- [Android capture](/quest/m1/mobile-capture-android.md) - native portrait capture
