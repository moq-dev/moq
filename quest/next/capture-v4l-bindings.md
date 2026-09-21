# [S] Capture without the V4L2 bindgen build dependency

## Goal

moq-video `capture` builds for every builder without libclang or kernel
headers at build time, and carries no runtime system requirement.

## Plan

The runtime side is already clean: the camera path drives the kernel through
ioctls, so no system library ships in the binary. What remains is build time.
Nix already covers in-tree builds (bindgenHook provides libclang), so vendor
or pin the `v4l` crate with checked-in bindings and drop its bindgen build
script, the way `moq-nvenc` does and
[VAAPI](/quest/next/video-vaapi.md) plans. That covers every other builder too
(`cargo install`, Docker). Keep the `v4l` API so `capture/v4l2.rs` and the
`v4l2` codec backend need no logic changes; fixing `v4l` fixes both features
since both drive the same device node through it.

Verify by building `just rs capture` with no libclang on PATH and by
inspecting the binary to prove no new runtime library requirement. The PR
3850 capture gate keeps the coverage.

## Related

- [Ship capture and playback](/quest/next/cli-packaging.md) - the shippable capture milestone this work supports
- [VAAPI encode and decode](/quest/next/video-vaapi.md) - same pre-generated bindings pattern for libva
- [Linux capture parity](/quest/next/capture-linux.md) - the Linux capture milestone this work supports
