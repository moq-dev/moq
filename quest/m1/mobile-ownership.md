# [XS] Rust owns codecs on mobile

## Goal

The mobile media boundary is written down: Rust owns capture, codecs, and
rendering, and `moq-ffi` bridges native surfaces (`CVPixelBuffer` on iOS,
Android `HardwareBuffer`/`Surface`) as opaque handles. Swift and Kotlin do not
grow a parallel platform codec stack.

## Plan

Decided in the 2026-09-30 audit: option 2 from #700, Rust owns codecs. The
tree already shipped it. `moq-ffi` defaults to the `audio` and `video`
features (`rs/moq-ffi/Cargo.toml:27`), so the Kotlin and Swift packages already
carry the Rust codecs, and #4094 added the `CVPixelBuffer` bridge
(`MoqVideoSurface::PixelBuffer` on `dev`). Option 1 would add a second media
stack beside one that exists.

What remains is recording the verdict in `rs/moq-ffi/AGENTS.md` (a maintainer
edit). [Android capture](/quest/m2/mobile-capture-android.md),
[iOS capture](/quest/m2/mobile-capture-ios.md), and the MediaCodec audio
quests already record it and no longer wait on this.
