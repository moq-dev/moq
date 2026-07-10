# moq-nvenc

Safe-ish Rust bindings for the NVIDIA Video Codec SDK (NVENC + NVDEC), vendored
for the MoQ workspace. `moq-video` uses the encoder path to hardware-encode
H.264/H.265 on Linux, and the `cuvid` table to hardware-decode via NVDEC.

This is a fork of [`nvidia-video-codec-sdk`](https://github.com/ViliamVadocz/nvidia-video-codec-sdk)
(MIT, Copyright Viliam Vadocz), trimmed to a single mode: it always dlopens the
driver libraries at runtime (`libnvidia-encode` for NVENC, `libnvcuvid` for
NVDEC) rather than linking them. So a binary built without
the NVIDIA driver still links on a GPU-less builder and starts on machines that
lack the driver (falling back to another encoder); the build needs no CUDA
toolkit or driver libs present.

The crate compiles on any platform, macOS included: the `sys` bindings are plain
C-ABI definitions and nothing links at build time. It only actually loads NVENC
on Linux (that is the only place `moq-video` calls it); elsewhere it is a
compile-only stub.

The `sys` bindings are generated with bindgen from the vendored headers
(`src/sys/headers/`); see the [upstream repo](https://github.com/ViliamVadocz/nvidia-video-codec-sdk)
for the generation scripts.

## License

MIT, inherited from upstream. See `LICENSE`.
