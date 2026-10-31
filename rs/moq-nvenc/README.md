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

## Upstream

Last checked on 2026-10-31: upstream `master` at
[`88c2ca67406a5087ac117758d0f375d73441c040`](https://github.com/ViliamVadocz/nvidia-video-codec-sdk/commit/88c2ca67406a5087ac117758d0f375d73441c040)
(2026-03-31), the base of the vendored `dynamic-loading` branch. Upstream
has no commits since that baseline and no
[published security advisories](https://github.com/ViliamVadocz/nvidia-video-codec-sdk/security/advisories),
so there are no wrapper fixes to port this run.

The vendored encoder header targets NVENC API 12.1. NVIDIA's current
[Video Codec SDK 13.1 release notes](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/read-me/index.html)
and [13.0 release notes](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/read-me/index.html)
were checked against `moq-video`'s consumers:

- AV1 hierarchical B-frames, UHQ iterative encoding, additional profiles and
  multiview support do not serve the current low-latency H.264/H.265 NV12
  encoder. It disables B-frames and lookahead. AV1 and Main10 already have
  [separate](../../quest/m3/nvenc-av1.md)
  [quests](../../quest/m3/2147-moq-video-10-bit-hevc-and-av1-support-in-the-nvidia-codec.md).
- Decode statistics, multiview metadata and sample-app seeking have no current
  consumer in the live H.264/H.265/AV1 decoder.
- Application-owned CUDA arrays could remove the decoder's device-to-device
  copy before NVENC. This is a candidate for a measured optimization, requiring
  new bindings, surface ownership and synchronization, rather than a small fix.
  See NVIDIA's
  [opaque decode output guide](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvdec-video-decoder-api-prog-guide/index.html).
- The [removed legacy presets and rate-control modes](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/deprecation-notices/index.html)
  do not affect the encoder's P1/P4/P7 presets with CBR. It does not request
  byte-based slices or use the deprecated video-source APIs.

No SDK header update is needed for those current consumers. An API 13.1
upgrade would raise the minimum driver requirement; NVIDIA documents driver
610 or newer for that SDK. The
[NVIDIA security bulletins](https://github.com/NVIDIA/product-security/tree/main/2026)
were also checked: GPU driver fixes belong in the installed driver, which this
crate loads but does not vendor. No applicable vendored-source security fix
was identified; updating headers does not update the driver.

## License

MIT, inherited from upstream. See `LICENSE`.
