# [S] Plan: who owns capture, codecs, and rendering on mobile

## Goal

A written decision, recorded in this tree, on the mobile media boundary:
either Swift and Kotlin own camera, platform codecs, and rendering while Rust
carries encoded access units through the existing media APIs, or Rust owns
capture, codecs, and rendering and `moq-ffi` grows opaque native surface
bridges for `CVPixelBuffer` and Android `HardwareBuffer`/`Surface`. The
capture quests in this questline start only once this is settled.

## Plan

- Option 1 is smaller and idiomatic for an app SDK; option 2 avoids two
  parallel media stacks and matches the in-tree direction of moq-video, at
  the cost of native-handle ownership across the FFI. #700 lays both out.
- Weigh with evidence rather than preference: what `moq-kit` already does in
  Kotlin over `moq-ffi`, what the iroh-live reimplementation says about the
  Rust-native audience, and the copy cost of byte-array frames measured on a
  device.
- Record the verdict here and in `rs/moq-ffi/AGENTS.md`, and re-estimate
  [Android capture](/quest/m2/mobile-capture-android.md) and
  [iOS capture](/quest/m2/mobile-capture-ios.md) against it; both target
  `moq-video`, so replace them with the required platform-owned implementation
  quests if the answer is option 1. Update [mobile completion](/quest/m2/mobile-completion.md)
  to require those replacements before abandoning the Rust capture quests.
