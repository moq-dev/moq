# moq-nvenc

Rust bindings for the NVIDIA Video Codec SDK (NVENC + NVDEC), vendored
for the MoQ workspace. `moq-video` uses the encoder path to hardware-encode
H.264/H.265 on Linux, and the `cuvid` table to hardware-decode via NVDEC.

This is a fork of [`nvidia-video-codec-sdk`](https://github.com/ViliamVadocz/nvidia-video-codec-sdk)
(MIT, Copyright Viliam Vadocz), trimmed to a single mode: it always dlopens the
driver libraries at runtime (`libnvidia-encode` for NVENC, `libnvcuvid` for
NVDEC) rather than linking them. So a binary built without
the NVIDIA driver still links on a GPU-less builder and starts on machines that
lack the driver (falling back to another encoder); the build needs no CUDA
toolkit or driver libs present.

Call `Encoder::load` to validate NVENC before creating a CUDA context. Missing
libraries and entry points, rejected loader calls, and drivers older than the
vendored SDK are reported as `LoadError` values. The safe encoder facade never
panics while loading its driver function table.

The crate compiles on any platform, macOS included: the `sys` bindings are plain
C-ABI definitions and nothing links at build time. It only actually loads NVENC
on Linux (that is the only place `moq-video` calls it); elsewhere it is a
compile-only stub.

The public encoder facade owns configuration and registered allocations, seals
driver handles, and returns a submission that keeps input and output resources
alive until synchronous completion. Raw SDK structs remain available under
`sys`; APIs that accept their pointers are explicitly unsafe.

External input registration is transactional. If NVENC registers an allocation
but cannot map it, the safe wrapper unregisters it before releasing its owner.
When rollback also fails, the mapping error remains primary, the unregister
error is available through `EncodeError::cleanup`, and the allocation stays
owned because the driver may still refer to it.

The `sys` bindings are generated with bindgen from the vendored headers
(`src/sys/headers/`); see the [upstream repo](https://github.com/ViliamVadocz/nvidia-video-codec-sdk)
for the generation scripts.

## License

MIT, inherited from upstream. See `LICENSE`.
