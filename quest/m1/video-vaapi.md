# [S] VAAPI encode and decode

## Goal

VAAPI encodes H.264 and H.265 from a DMA-BUF without a download, decodes
H.265 as well as H.264, and the `vaapi` feature costs a consumer nothing at
build time so it can return to default-on. A resize reuses its output
surfaces.

## Plan

The crate work is in four moq-dev/vaapi PRs:

- [moq-dev/vaapi#8](https://github.com/moq-dev/vaapi/pull/8): checked-in libva bindings, so no build script and no libclang.
- [moq-dev/vaapi#9](https://github.com/moq-dev/vaapi/pull/9): `Processor::resize` into output surfaces pooled per size.
- [moq-dev/vaapi#10](https://github.com/moq-dev/vaapi/pull/10): H.265 Main encode beside H.264.
- [moq-dev/vaapi#11](https://github.com/moq-dev/vaapi/pull/11): H.265 Main decode beside H.264, stacked on #9.

The moq side is on this quest's branch: `vaapi` on by default in moq-video,
moq-transcode, and moq-cli, H.265 encode and decode in moq-video's VAAPI
backends, `Surface::resize` through the pool, and no bindgen hook in moq-c's
Nix build.

What remains is the release and the bump. The four PRs together break the
crate's API (`encode::probe` and `decode::probe` take a codec, both `Config`s
gain a `codec` field, and `Processor::process` goes private), so the release is
0.2.0. Then bump `moq-vaapi` in the workspace `Cargo.toml` and `Cargo.lock` on
this quest's branch and rerun its checks; until then the branch does not build
against 0.1.1.

## Waiting

- Maintainer: merge moq-dev/vaapi#8 through #11 and publish the moq-vaapi release, since agents do not cut releases. Check with `cargo search moq-vaapi`.

## Related

- [VA-API encodes an external Vulkan image](/quest/m3/vaapi-vulkan-import.md) - explicit-sync DMA-BUF input and render-node selection by device, H.264 only
