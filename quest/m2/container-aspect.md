# [S] Container aspect signals

## Goal

The fMP4 `pasp` box and Matroska `DisplayWidth`/`DisplayHeight` feed the
catalog's display aspect (`display_aspect_width`/`display_aspect_height`), so
AV1 renditions and headerless avc3/hev1 initializers that carry them import
with the right display shape. Today only SPS metadata sets it (#5755).

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-ups of [#5755](https://github.com/moq-dev/moq/pull/5755).

The two signals mean different things, so each maps on its own:

- fMP4 `pasp` is a pixel aspect (`hSpacing:vSpacing`, like the SPS sample
  aspect). The display aspect is the picture's width times `hSpacing` to its
  height times `vSpacing`. A 1:1 `pasp` adds nothing beyond the coded size, so
  it leaves the field unset, as a square SPS does since #5755. mp4-atom
  already parses `pasp` on visual sample entries.
- Matroska `DisplayWidth`/`DisplayHeight` are display dimensions after
  cropping, so their ratio is the display aspect itself. Set it whenever it
  differs from the coded `PixelWidth`/`PixelHeight` ratio, even when it is
  1:1. Every `DisplayUnit` gives a ratio, since both fields share the unit:
  pixels, centimeters, inches, or a bare aspect ratio. With `DisplayUnit` 0,
  an absent field defaults to the cropped size (`PixelWidth` minus
  `PixelCropLeft` and `PixelCropRight`, `PixelHeight` minus `PixelCropTop`
  and `PixelCropBottom`), so an uneven crop alone can change the aspect. Any
  other unit requires both fields, so a track missing one fails the import
  as malformed.

When the container and the SPS both signal an aspect and disagree, propose a
precedence in the PR. Recommendation: the container wins, as FFmpeg's
`av_guess_sample_aspect_ratio` prefers the stream's ratio over the codec's.

Tests, through both importers, including an AV1 track and a headerless avc3
initializer:

- 1440x1080 with `pasp` 4:3 imports as 16:9.
- 1440x1080 with `pasp` 1:1 leaves the field unset.
- Matroska 1440x1080 with a 1920x1080 display imports as 16:9.
- Matroska 1440x1080 with a 1080x1080 display imports as 1:1.
- Matroska 1440x1080 with `PixelCropRight` 360 and no display dimensions
  imports as 1:1.
- Matroska with a 16x9 display in centimeters or inches imports as 16:9.
- Matroska with a non-pixel `DisplayUnit` and no display dimensions fails
  the import.
- Matroska without crop or display dimensions, or with ones matching the
  pixel ratio, leaves the field unset.

In-band SPS acquisition for headerless initializers is separate work.
