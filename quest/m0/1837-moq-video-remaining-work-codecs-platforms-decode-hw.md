# [XL] moq-video: remaining work (codecs, platforms, decode, HW validation)

## Goal

Implement and verify the behavior tracked in [#1837](https://github.com/moq-dev/moq/issues/1837)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Tracking issue for the gaps in `moq-video` after the ffmpeg removal + native-codec rework on `dev`. Audit done against `rs/moq-video` on the `dev` branch.

Current state:

- **Encode**: H.264 via openh264 (software, all platforms), VideoToolbox (macOS), Media Foundation (Windows), NVENC + VAAPI (Linux, feature-gated; NVENC HW-validated on an RTX 3070 Ti, VAAPI still unverified). H.265 via VideoToolbox (macOS), Media Foundation (Windows), and NVENC (Linux, feature-gated, HW-validated).
- **Decode**: H.264 via VideoToolbox (macOS), Media Foundation/DXVA (Windows), openh264 (software). H.265 via Media Foundation/DXVA (Windows) and VideoToolbox (macOS, #1859); no software fallback. H.264/H.265/AV1 via NVDEC (Linux, #2145 / #2178), with zero-copy NVDEC to NVENC transcode and GPU resize fanout (#2158, `moq transcode`). In `[Unreleased]`.
- **Capture**: camera on all three platforms (AVFoundation / V4L2 / Media Foundation); screen capture on macOS (ScreenCaptureKit) and Windows (DXGI Desktop Duplication).

#### Building on top of moq-video (renderer, mobile, universal zero-copy, public frame API)

Motivation, from an audit of downstream consumers: n0's `iroh-live` (Rust, `wgpu` renderer, iroh P2P) and Software Mansion's `moq-kit` (native Swift/Kotlin mobile SDK on `moq-ffi`) both reimplement the raw-frame media layer `moq-video` already owns. They have to, because `moq-video` today exposes zero-copy *ingest* (capture -> encode on macOS/Windows) but no zero-copy *egress*: a decoded `Frame` can only leave as CPU I420 (`into_i420()`) or be fed back into our own encoder (the transcode path). There is no way to get the GPU handle out, and no renderer. So `decode -> render` forces a CPU download + re-upload (a double copy), which is the exact inverse of the ingest path.

Closing the items below lets a consumer build a renderer or an alternate encoder/filter *on top of* `moq-video` instead of forking the whole native layer. It also gives us end-to-end zero-copy in one crate (we currently have only the front half; `iroh-live` has only the back half).

##### Frame surface API (get the GPU handle out)

- \[ ] **Expose a public, platform-tagged surface handle** on `decode::Frame` (and the shared internal `Frame`). Today `Frame.inner` is `pub(crate)`. Add a `#[non_exhaustive]` enum of *borrowed* handles: `CVPixelBuffer` (macOS/iOS), `ID3D11Texture2D` (Windows), CUDA device pointer (Linux/NVDEC), dmabuf FD (Linux/VAAPI), CPU I420. Codec- and backend-agnostic; asking for a handle the frame doesn't hold is a typed error, never UB. This is an insulated building block (Public API Scrutiny): a renderer or a third-party encoder snaps on without `moq-video` owning either.
- \[ ] **Symmetric typed conversions**, mirroring `into_i420()`: `into_cuda()`, `to_cvpixelbuffer()` / `into_metal()`, `into_d3d11()`, `into_dmabuf()`. Each is a cheap borrow/wrap when the frame already lives on that device, and an explicit upload/download otherwise, so the copy cost is visible at the call site rather than hidden. Prefer consuming `self` where a copy would otherwise be implied.
- \[ ] **Make decoded frames stay on the GPU first.** macOS/Windows decode currently lands as CPU I420 (the `resize` code even notes "capture-only surfaces never appear in decoded frames"), so there is nothing to expose yet: VideoToolbox / Media Foundation decode must produce a GPU `Frame` before the accessor above means anything. NVDEC already does (`Frame::Cuda`).
- \[ ] **The handle API is still worth it even with an in-tree renderer.** The renderer lives in `moq-video` (see Rendering), but the public surface handle is what lets a consumer build its *own* encoder/filter/renderer on top without forking the native layer, so it is not redundant with the built-in renderer.

##### Rendering

`moq-video` stops at a decoded `Frame`; there is no present path, so every consumer downloads to I420 and re-uploads to its own texture. The renderer lives **in `moq-video`** as a `render` role module, symmetric with `capture` / `encode` / `decode` (the crate already owns the platform native layer on both ends, so rendering belongs here too, not in a sibling crate). External consumers that want their own present path still can, via the Frame surface API above.

- \[ ] **`render` module**, mirroring `capture`: a `render::Config` + `render::Sink` (or similar) that takes decoded `Frame`s and presents them, picking the platform-native path.
- \[ ] **Zero-copy present paths** built on the surface API above: `CVPixelBuffer` -> `CVMetalTextureCache` -> Metal (macOS/iOS); CUDA / dmabuf -> GL/Vulkan (Linux); D3D11 shared texture -> swapchain (Windows); `SurfaceTexture` -> GL/Vulkan (Android).
- \[ ] **Portable `wgpu` fallback** that samples NV12/I420 -> RGB on the GPU (color-matrix aware, BT.601/709); one shader covers all four backends (cf. `iroh-live`'s `nv12_to_rgba.wgsl`), the same way `openh264` is the cross-platform encode fallback.
- \[ ] **Color management** at sample time, not a CPU convert.

##### Mobile capture + encode

- \[ ] **iOS** - AVFoundation camera + ReplayKit screen. VideoToolbox encode/decode is already the macOS backend, so this is capture wiring + the permission / on-demand lifecycle flow, not a new codec backend. (Promoted from the footnote under Other platform coverage.)
- \[ ] **Android** - Camera2/CameraX + MediaProjection screen + MediaCodec encode/decode (Surface in/out, natively zero-copy). A whole new backend family (NDK/JNI rather than objc2). Weigh this against the fact that `moq-kit` already does it in Kotlin and raw frames cannot cross the `moq-ffi` boundary zero-copy, so a Rust Android media path only pays off for Rust-native consumers.

##### Universal zero-copy capture input

Ingest is zero-copy on macOS (`CVPixelBuffer` -> VideoToolbox) and Windows (D3D11 NV12 -> Media Foundation), but not Linux.

- \[ ] **Linux V4L2 / PipeWire** currently convert to I420 on the CPU (YUYV / BGRA). Add V4L2 `VIDIOC_EXPBUF` dmabuf export and PipeWire dmabuf negotiation, feeding a `Frame::DmaBuf` straight into VAAPI/NVENC. Pairs with the "VAAPI zero-copy dmabuf input path" item under Hardware validation.

##### Decode

- \[x] **H.264 hardware decode on Windows** (Media Foundation) - done in code (DXVA: NVDEC / Intel / AMD; GPU-less host falls back to openh264). In `[Unreleased]`; HW-validation pending.
- \[x] **H.264 hardware decode on Linux** (NVDEC) - done in #2145 (dlopen'd cuvid; GPU-less host falls back to openh264); HW-validated on an RTX 3070 Ti (round-trip + resize tests). VAAPI / V4L2 decode not implemented, NVDEC only.
- \[x] **H.265 hardware decode on macOS** (VideoToolbox) - done in #1859 (verified end-to-end on Apple M4).
- \[x] **H.265 hardware decode on Windows** (Media Foundation) - done in code (DXVA). In `[Unreleased]`; HW-unverified (test box had no HEVC decoder MFT installed).
- \[x] **H.265 hardware decode on Linux** (NVDEC) - done in #2145; HW-validated on an RTX 3070 Ti (NVENC encode to NVDEC decode round-trip). VAAPI decode still absent.
- \[ ] **H.265 software decode** - only if a backend with acceptable real-time performance and no licensing concerns exists; otherwise skip (do NOT add software HEVC decode just to fill the matrix)

##### Encode

- \[x] **Linux NVENC H.265** - done in #1840 (codec GUID selected from `Codec`; HW-validated on an RTX 3070 Ti)
- \[ ] **Linux VAAPI H.265** - VAAPI backend advertises H.264 only today. Update: `moq-vaapi` 0.0.2 vendors HEVC buffer types (`src/buffer/hevc.rs`) but its `Encoder` is hardcoded H.264 (`VAProfileH264Main` / `VAEntrypointEncSlice`), so exposing an HEVC encoder is a `moq-dev/vaapi` change, not just a moq-video flag.
- \[ ] ~~Software H.265 encode~~ - explicitly out of scope (no software HEVC encode)

##### Robustness

- \[x] **NVENC driver-probe (avoid abort on GPU-less box)** - done in #1844. `Auto` aborted under `panic = "abort"` when libcuda was missing because cudarc/the SDK `panic!` on a failed dlopen instead of erroring; `Nvenc::open` now probes libcuda / libnvidia-encode first and falls back cleanly.
- \[ ] **VAAPI: make `moq-vaapi` dlopen libva (not a driver-probe)** - the original premise was wrong. `moq-vaapi` 0.0.2 **links** libva (binary carries `NEEDED libva.so.2` / `libva-drm.so.2`, verified via `readelf`), it does **not** `dlopen` it, and its `Encoder::new` returns a `Result` rather than panicking. So unlike NVENC there is no panic-on-miss to guard: a libva-less box fails at process load (before any Rust probe could run), and the no-GPU case (libva present, no usable VA driver) already falls back cleanly via `Encoder::new` → `Err` → `backend::open` → openh264. A probe in `open()` can't help. **Mitigated in #1860 (merged):** corrected the docs that wrongly claimed dlopen, and made `nvenc`/`vaapi` default-on opt-out features so a self-compiler can drop the libva/CUDA deps (`cargo build -p moq-cli --no-default-features --features "iroh quinn websocket capture"`). **Still open:** the real fix is restoring the documented `vaapi_dlopen` design (no `cargo:rustc-link-lib`, no `DT_NEEDED`) in the separate `moq-dev/vaapi` crate, then adding the NVENC-style probe  -  only then does an always-on VAAPI binary load on a libva-less host.

##### Hardware validation (written but never run on real HW)

- \[x] **NVENC encode on a Linux + NVIDIA box** - done on an RTX 3070 Ti (driver 595.71.05): pitch alignment, periodic forced-IDR at the GOP boundary, and H.265 VPS/SPS/PPS inline ahead of each IDR all pass. Gotcha fixed en route: NVENC rejects stream-ordered pool memory (`cuMemAllocAsync`), so buffers registered with NVENC come from plain `cuMemAlloc`.
- \[ ] **VAAPI encode on a Linux + Intel/AMD box** - low-power vs full entrypoint, NV12 upload round-trip, `cargo deny` license resolution
- \[ ] **VAAPI zero-copy dmabuf input path** - currently uses an NV12 surface-upload; add the `Frame::DmaBuf` + V4L2 `VIDIOC_EXPBUF` path for NV12-capable sources
- \[ ] **Windows Media Foundation capture on real HW** - on-demand open/close (LED off when unwatched), NV12 delivery for MJPEG/YUY2 cameras
- \[ ] **Live camera run per platform** - capture needs device permission a headless/agent process can't get

##### Capture

- \[ ] **Screen/display capture on Linux** (PipeWire/portal, or X11/DRM)
- \[x] **Screen/display capture on Windows** (Desktop Duplication) - done in code (DXGI Desktop Duplication, whole-monitor). In `[Unreleased]`; compile-verified, HW-validation pending.
- \[ ] **Mobile capture** - see **Mobile capture + encode** above (iOS AVFoundation/ReplayKit; Android Camera2/MediaProjection).
- \[ ] **Zero-copy capture input on Linux** - see **Universal zero-copy capture input** above (V4L2 `VIDIOC_EXPBUF` / PipeWire dmabuf).

##### Codecs (new)

- \[x] **AV1 hardware decode** (NVDEC) - done in #2178 (8-bit 4:2:0 only, hardware-only, catalog-shape gated).
- \[ ] **AV1 encode** - returns when a hardware backend lands (NVENC/VAAPI AV1 on Linux). No software AV1 encode (rav1e too slow for real-time). Software AV1 decode only if perf + licensing are acceptable. `Codec` enum is `#[non_exhaustive]` so this is additive.
- \[ ] VP8 / VP9 - the `hang` catalog models them but `moq-video` has no path either way; track here, low priority

##### Other platform coverage (noted in DESIGN, lower priority)

- \[ ] Intel QSV-specific encode path (Intel currently goes through VAAPI)
- \[ ] Raspberry Pi `v4l2m2m` stateful encoder
- \[ ] iOS / Android media path - promoted to **Mobile capture + encode** above (iOS reuses the VideoToolbox backend; Android is a new MediaCodec/NDK family)

##### Docs

- \[x] **Fix the stale `rs/moq-video/README.md`** - done in #1840 (removed the ffmpeg / system-FFmpeg / "decode not implemented" claims; added a capability matrix).

---

Constraints captured from the request: do **not** add software H.265/AV1 *encode* at all; only add software H.265/AV1 *decode* if a backend has good enough real-time performance and no licensing concerns.

## Closes

- [#1837](https://github.com/moq-dev/moq/issues/1837) - close this issue when the quest finishes
