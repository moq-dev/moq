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
  differs from the pixel dimensions' ratio, even when it is 1:1. Mind
  `DisplayUnit`: pixels and display aspect ratio both give a ratio here, and
  an absent element defaults to the pixel size, which adds nothing.

When the container and the SPS both signal an aspect and disagree, propose a
precedence in the PR. Recommendation: the container wins, as FFmpeg's
`av_guess_sample_aspect_ratio` prefers the stream's ratio over the codec's.

Tests, through both importers, including an AV1 track and a headerless avc3
initializer:

- 1440x1080 with `pasp` 4:3 imports as 16:9.
- 1440x1080 with `pasp` 1:1 leaves the field unset.
- Matroska 1440x1080 with a 1920x1080 display imports as 16:9.
- Matroska 1440x1080 with a 1080x1080 display imports as 1:1.
- Matroska without display dimensions, or with ones matching the pixel
  ratio, leaves the field unset.

In-band SPS acquisition for headerless initializers is separate work.
