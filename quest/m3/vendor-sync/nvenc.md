# [XS] moq-nvenc has every upstream fix it needs

## Goal

moq-nvenc carries every fix
[ViliamVadocz/nvidia-video-codec-sdk](https://github.com/ViliamVadocz/nvidia-video-codec-sdk)
has landed since the last check, each ported with its test or recorded as not
applying, and `rs/moq-nvenc/README.md`'s `Upstream` section names the newest
upstream commit checked.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs.

Last checked (2026-10-31): upstream `master` at
[`88c2ca67`](https://github.com/ViliamVadocz/nvidia-video-codec-sdk/commit/88c2ca67406a5087ac117758d0f375d73441c040)
(2026-03-31), the base of the vendored `dynamic-loading` branch, with no newer
commits and no published advisories. NVIDIA's Video Codec SDK 13.0 and 13.1
and its 2026 security bulletins were checked against moq-video's consumers;
the README records why no header update was needed (API 13.1 would raise the
minimum driver to 610).

Each run also checks for a Video Codec SDK newer than 13.1, against the
encoder's low-latency H.264/H.265 NV12 path and the live decoder. Driver
security fixes belong in the installed driver, which this crate loads but does
not vendor.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
