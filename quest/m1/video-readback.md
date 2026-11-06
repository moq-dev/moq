# [S] Read a texture back without an encoder

## Goal

moq-video exposes a public readback that copies a wgpu texture into tightly
packed RGBA rows on the CPU with no encoder behind it, so a consumer that
needs the raw picture reuses the export path's staging buffers, row
unpadding, and mapping lifecycle instead of copying them. A moq-side test
covers it.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): the
studio's [Linux virtual camera](https://github.com/moq-dev/studio/blob/main/quest/m0/vcam/linux.md)
([moq-dev/studio#9](https://github.com/moq-dev/studio/pull/9)) reads the
program through this readback rather than a copy of it in the studio, and the
work joins m1 because it is additive and that camera has waited on it since
2026-11-02. The studio bumps its pin in
[its own quest](https://github.com/moq-dev/studio/blob/main/quest/m0/pin-readback.md).

The readback exists: `render/export.rs`'s private `Readback` and its test
`readback_round_trips_and_drops_past_its_depth`. Today it is reachable only
through `Exporter`, whose setup probes for an encoder. Make that type public
and keep one implementation, so `Exporter`'s CPU route still reads through it.

Recommendation, not settled: mirror `Exporter`'s shape, a non-blocking copy
that drops past its depth and returns a pending handle whose wait yields the
RGBA bytes, named by role under `render`. Refuse a texture whose size or
format does not match, as `Exporter::copy` does. Add a `MIGRATE.md` note
under `## Unreleased`.
