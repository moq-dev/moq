# [M] moq-cade encodes cleanly

## Goal

moq-cade's video looks clean at the size the web player shows it, and the
harness keeps it that way. Rows score clips generated from the emulator,
measured before and after the fix.

## Plan

Decided in planning (2026-10-28): the clips are generated, not stored. Run the
emulator headless on public homebrew ROMs from `rom.moq.dev` with scripted
input, and write Y4M: one clip with heavy motion and one calm scene per
console. Only the ROM fetch touches the network.

Retargeted 2026-11-24 (maintainer, /quest-plan of
[moq-cade](/quest/m4/cade/README.md)): MoQ Boy's Game Boy is replaced by
libretro cores for NES (256x240), SNES (256x224, and 512 wide in hi-res), and
Genesis (320x224). The 160x144 frame was below the hardware encoders'
minimum, which is why the encoder ran OpenH264 in camera mode at about 245
kbps with a 2 s GOP. Re-read `rs/moq-cade` for what it does once
[libretro](/quest/m4/cade/libretro.md) lands.

Score the video the way the player displays it, at a size fixed in the
harness rather than read from a window. Upscale the reference and the
decoded output by one integer factor with nearest neighbor, picking the
factor closest to VMAF's 1080p model (4x for NES and SNES), and run VMAF.
Report PSNR at native size alongside.

Candidates to measure:

- 4:2:0 chroma at native size smears the edges of colored pixels. A 2x
  nearest-neighbor upscale before encoding keeps full chroma for each console
  pixel.
- OpenH264's `ScreenContentRealTime` usage type, which suits flat pixel art,
  against the hardware backends.
- Bitrate and GOP length, now that the pop metric can see keyframe pumping.

If the winner needs a content hint in `moq_video::encode::Config`, stop and
bring the API shape to the maintainer.

Public API: possibly, see above. Wire: none.

## Required

- [Harness](/quest/m4/video-quality/harness.md) - the scorer this fix is measured with
- [libretro](/quest/m4/cade/libretro.md) - the emulator whose output this scores
