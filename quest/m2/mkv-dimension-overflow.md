# [XS] MKV import refuses out-of-range values

## Goal

The Matroska importer (`rs/moq-mux/src/container/mkv/import.rs`) refuses a
pixel width or height, channel count, or sampling frequency that does not fit
its catalog field with an import error, instead of truncating it with
`as u32`. Regression tests cover each field.

## Plan

From the [#5893](https://github.com/moq-dev/moq/pull/5893) follow-ups.
Today `coded_width` and `coded_height` come from the track's pixel width and
height through `as u32`, and `Channels` and `SamplingFrequency` the same way.

Decided 2026-10-19 (maintainer): truncation only. Dropped: matching the SPS
aspect ratio 1:1, since FFmpeg prefers the container SAR and we follow
FFmpeg; and a catalog crop field, since nothing needs one.

Public API: none beyond a new import error case. Wire: none.
