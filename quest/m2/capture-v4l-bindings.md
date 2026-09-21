# [S] Capture without the V4L2 bindgen build dependency

## Goal

moq-video `capture` builds on Linux without libclang or kernel headers, so the
camera path stops blocking capture-by-default.

## Plan

Vendor or pin the `v4l` crate with checked-in bindings and drop its bindgen
build script, the way `moq-nvenc` does and
[VAAPI](/quest/m2/video-vaapi.md) plans. Keep the `v4l` API so
`capture/v4l2.rs` and the `v4l2` codec backend need no logic changes. Fixing
`v4l` fixes both features since both drive the same device node through it.

Verify by building `just rs capture` on a host without libclang and by
inspecting the dependency graph to prove bindgen is gone. The PR 3850 capture
gate keeps the coverage.

## Related

- [Ship capture and playback](/quest/m2/cli-packaging.md) - shippable capture needs this first
- [VAAPI encode and decode](/quest/m2/video-vaapi.md) - same pre-generated bindings pattern for libva
- [Linux capture parity](/quest/m2/capture-linux.md) - the capture surface this unblocks
