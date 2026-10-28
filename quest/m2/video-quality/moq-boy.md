# [M] moq-boy encodes cleanly

## Goal

moq-boy's video looks clean at the size the web player shows it, and the
harness holds it there. Rows score clips generated from the emulator,
measured before and after the fix.

## Plan

Decided in planning (2026-10-28): the clips are generated, not stored. Run the
emulator headless on public homebrew ROMs from `rom.moq.dev` (for example
Dangan GB2 for motion and Big2Small for calm scenes) with scripted input, and
write Y4M. Only the ROM fetch touches the network.

Today (`rs/moq-boy/src/video.rs`) moq-boy encodes the native 160x144 frame at
60 fps through OpenH264 in camera mode. Hardware encoders refuse a frame that
small. The bitrate is the 0.07 bits-per-pixel default (about 97 kbps), with a
2 s GOP.

Score the way the player displays the video. Upscale the reference and the
decoded output with nearest neighbor to the player's display size, then run
VMAF. Report PSNR at native size alongside.

Candidates to measure:

- 4:2:0 chroma at 160x144 smears colored pixel edges. Upscaling by 2x or 4x
  with nearest neighbor before encoding keeps full chroma per Game Boy pixel,
  and also lifts the frame above hardware encoders' minimum size.
- OpenH264's `ScreenContentRealTime` usage type, which suits flat pixel art.
- Bitrate and GOP length, now that the pop metric can see keyframe pumping.

If the winner needs a content hint in `moq_video::encode::Config`, stop and
bring the API shape to the maintainer.

Public API: possibly, see above. Wire: none.

## Required

- [Harness](/quest/m2/video-quality/harness.md) - the scorer this fix is measured with
