# moq-nvenc

Safe-ish Rust bindings for the NVIDIA Video Codec SDK (NVENC), vendored for the
MoQ workspace. `moq-video` uses the encoder path to hardware-encode H.264/H.265
on Linux.

This is a fork of [`nvidia-video-codec-sdk`](https://github.com/ViliamVadocz/nvidia-video-codec-sdk)
(MIT, Copyright Viliam Vadocz). The one addition is the `dynamic-loading`
feature: it dlopens `libnvidia-encode` at runtime instead of linking it, so a
binary built without the NVIDIA driver still links on a GPU-less builder and
starts on machines that lack the driver (falling back to another encoder). Under
that feature the build needs no CUDA toolkit or driver libs present.

The crate is Linux/Windows only (`src/sys` has no macOS bindings), so it is
excluded from the workspace and pulled in as a path dependency only where a
target actually needs it.

The `sys` bindings are generated with bindgen; see `src/sys/linux_sys/bindgen.sh`
and `src/sys/windows_sys/bindgen.ps1` to regenerate from the vendored headers.

## License

MIT, inherited from upstream. See `LICENSE`.
