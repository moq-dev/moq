# [XS] moq-nvenc has every upstream fix it needs

## Goal

moq-nvenc carries every fix
[ViliamVadocz/nvidia-video-codec-sdk](https://github.com/ViliamVadocz/nvidia-video-codec-sdk)
has landed since the last check, each ported with its test or recorded as not
applying, and `rs/moq-nvenc/README.md` gains an `Upstream` section naming the
newest upstream commit checked.

## Plan

Recurring child of the [vendor sync](/quest/m1/vendor-sync/README.md), which
says how each run works and recurs.

Last checked: upstream `master` at
[`88c2ca67`](https://github.com/ViliamVadocz/nvidia-video-codec-sdk/commit/88c2ca67406a5087ac117758d0f375d73441c040)
(2026-03-31), the base of the `dynamic-loading` branch vendored in
[moq-dev/moq#2042](https://github.com/moq-dev/moq/pull/2042). It was still
upstream's head on 2026-10-30, so the first run likely only writes the
`Upstream` section. Also check whether NVIDIA released a Video Codec SDK newer
than the vendored headers that a moq encoder or decoder would use.
