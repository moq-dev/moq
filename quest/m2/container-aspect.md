# [S] Container aspect signals

## Goal

The fMP4 `pasp` box and Matroska `DisplayWidth`/`DisplayHeight` feed the
catalog's display aspect (`display_aspect_width`/`display_aspect_height`), so
AV1 renditions and headerless avc3/hev1 initializers that carry them import
with the right display shape. Today only SPS metadata sets it (#5755).

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-ups of [#5755](https://github.com/moq-dev/moq/pull/5755).

When the container and the SPS both signal an aspect and disagree, propose a
precedence in the PR. Recommendation: the container wins, as FFmpeg's
`av_guess_sample_aspect_ratio` prefers the stream's ratio over the codec's.
A square ratio leaves the field unset, as #5755 does. Mind Matroska's
`DisplayUnit`. mp4-atom already parses `pasp` on visual sample entries.

Tests: non-square and square fixtures through both importers, including an AV1
track and a headerless avc3 initializer. In-band SPS acquisition for
headerless initializers is separate work.
