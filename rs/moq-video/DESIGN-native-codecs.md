# Native H.264 codecs for moq-video (drop ffmpeg)

Status: **phases 2-6 implemented** (openh264, VideoToolbox, NVENC, VAAPI, capture
swap + ffmpeg removal). Capture is now native on all three platforms (AVFoundation
/ ScreenCaptureKit on macOS, V4L2 on Linux, Media Foundation on Windows), so
**nokhwa is fully removed**. VAAPI is back via discord/cros-codecs with an NV12
surface-upload input path; the zero-copy dmabuf capture is a follow-up. See "As
built" at the bottom for where the implementation diverged from this plan.

## Goal

Remove `ffmpeg-next` from `moq-video` entirely and drive H.264 capture + encode
through native, per-platform crates instead. The point is **packaging**: a single
statically self-contained binary that still reaches the GPU at runtime, so we can
ship one `.deb` / `.rpm` / brew bottle per arch instead of one per distro release.

### Why ffmpeg blocks this today

`moq-video` links ffmpeg in two spots, both of which have to go:

- Encode: [`encode/encoder.rs`](src/encode/encoder.rs) probes `h264_videotoolbox` /
  `h264_nvenc` / `h264_vaapi` / `libx264` by name.
- Capture + color convert: [`capture.rs`](src/capture.rs) uses `libavdevice`
  (avfoundation / v4l2 / dshow) and `swscale` for the camera frame -> YUV420P step.

Static-linking ffmpeg drops hardware support (the `h264_*` encoders themselves
`dlopen` vendor driver libs, and bundling that is painful). Dynamic-linking
ffmpeg forces a package per distro release because of `libav*` soname churn.
Native crates sidestep both: the hardware paths `dlopen` the vendor driver
at runtime, so we link nothing heavy at build time.

**Key constraint:** swapping only the encoder gains nothing for packaging while
capture still pulls in `libav*`. This proposal replaces encode **and** capture.

### Scope (agreed)

- Platforms: macOS (VideoToolbox), Linux NVIDIA (NVENC), Linux Intel/AMD (VAAPI).
- Out of scope: Windows (AMF/QSV), iOS, HEVC/AV1. H.264 only.

## Crate selection

| Backend | Crate | Role | Linking model |
|---|---|---|---|
| VideoToolbox (macOS) | [`objc2-video-toolbox`](https://docs.rs/objc2-video-toolbox/) + `objc2-core-media` / `objc2-core-video` | Raw FFI. We hand-write the `VTCompressionSession` glue. | System frameworks, always present. Zero external runtime deps. |
| NVENC (NVIDIA) | `moq-nvenc` (in-tree `rs/moq-nvenc`; fork of [`nvidia-video-codec-sdk`](https://crates.io/crates/nvidia-video-codec-sdk) 0.4 trimmed to dlopen-only) | Safe `Encoder` wrapper. | NVENC API lives in the driver (`libnvidia-encode.so`), `dlopen`'d at runtime. No build-time SDK linking. |
| VAAPI (Intel/AMD) | [`moq-vaapi`](https://crates.io/crates/moq-vaapi) `0.0.2` (published; vendored+trimmed from cros-libva + discord/cros-codecs) | VAAPI H.264 encoder (Google/ChromeOS, ships in crosvm). | As of 0.0.2 *links* `libva` (`NEEDED libva.so.2`), build needs libva-dev; `dlopen` (no NEEDED, no build dep) is intended but not yet realized, see #1837. |
| Software fallback | [`openh264`](https://crates.io/crates/openh264) | Pure fallback when no GPU. | Vendored build -> static, zero runtime deps. |

Decisions and rationale:

- **NVENC: buy, then vendor.** `nvidia-video-codec-sdk` is an independently-maintained
  safe wrapper; don't roll our own. We vendor a fork in-tree as `moq-nvenc`
  (`rs/moq-nvenc`), trimmed to always dlopen `libnvidia-encode` instead of linking
  it, so a driverless / GPU-less build still compiles and starts.
- **VAAPI: buy `cros-codecs`, but expect the heaviest integration.** It is the
  only credible non-ffmpeg VAAPI encode crate, and the H.264 VAAPI encoder is
  real and shipped in the published **0.0.6** (June 2025), not just `main`. See
  the dedicated notes below -- the catch is ergonomics, not capability.
- **VideoToolbox: build the session glue.** No trustworthy high-level crate exists.
  `objc2-video-toolbox` gives us safe, maintained bindings to
  `VTCompressionSession` and the property keys (`kVTCompressionPropertyKey_*` for
  bitrate / realtime / max-keyframe-interval / profile). We write ~300 lines of
  lifecycle + callback + format-description handling on top. This is glue, not a
  codec reimplementation.
- **Software fallback: `openh264`**, vendored so it links static. This guarantees
  a working binary on a machine with no usable GPU, with no runtime dependency.
  Tradeoff vs `libx264`: openh264 is constrained-baseline-ish and lower quality,
  but it is BSD-licensed and trivially static.

## Target architecture

Today `Encoder` is a monolith wrapping one ffmpeg encoder. Replace it with a
small trait and one impl per backend, keeping the existing `Kind`-driven
selection + linear fallback chain (which is already the right shape, see
[`encoder_candidates`](src/encode/encoder.rs)).

```rust
// encode/backend/mod.rs
pub(crate) trait Backend: Send {
    /// Encode one NV12 frame. `force_keyframe` requests an IDR. Returns zero or
    /// more H.264 packets in this backend's native framing (see `wire_mode`):
    /// Annex-B for Avc3 backends, length-prefixed NALs for Avc1.
    fn encode(&mut self, frame: &Nv12, force_keyframe: bool) -> Result<Vec<Bytes>, Error>;
    fn finish(&mut self) -> Result<Vec<Bytes>, Error>;

    /// Which `moq_mux::h264::Mode` the producer should run for this backend.
    /// Avc3 (Annex-B, in-band SPS/PPS) for openh264/nvenc/vaapi; Avc1 for VT.
    fn wire_mode(&self) -> Mode;
    /// For Avc1 only: the avcC (AVCDecoderConfigurationRecord) to hand
    /// `Import::initialize`, available once the first frame has been encoded.
    /// `None` for Avc3 backends.
    fn description(&self) -> Option<Bytes>;

    fn name(&self) -> &'static str;
}
```

Module layout under `src/encode/`:

```text
encode/
  encoder.rs        # public Encoder: picks a Backend via Kind, owns the color
                    # converter, exposes the unchanged encode_rgba / encode API
  backend/
    mod.rs          # Backend trait + open_backend(kind, config) fallback chain
    videotoolbox.rs # cfg(target_os = "macos")
    nvenc.rs        # cfg(target_os = "linux")
    vaapi.rs        # cfg(target_os = "linux")
    openh264.rs     # software fallback, all platforms
```

The public surface (`Encoder`, `Config`, `Kind`, `Producer`, `Options`,
`publish_capture`) stays **identical**, so `moq-cli` and the catalog/producer
path don't change. `Kind::Named(String)` keeps working but its meaning shifts
from "ffmpeg encoder name" to "backend id" (`"videotoolbox"`, `"nvenc"`,
`"vaapi"`, `"openh264"`); document the change.

### Framing: Import takes both Avc3 and Avc1, so no normalization

`moq_mux::codec::h264::Import` supports **both** wire formats via its `Mode`
([`import.rs:18`](../moq-mux/src/codec/h264/import.rs)):

- `Mode::Avc3` -> Annex-B (start-code framed), SPS/PPS **in-band**. Catalog
  `H264 { inline: true }`, no `description`.
- `Mode::Avc1` -> length-prefixed NALs (AVCC), SPS/PPS **out-of-band** supplied
  once as the `AVCDecoderConfigurationRecord` to `initialize()`. Catalog
  `H264 { inline: false }`, `description = avcC`.

This means each backend feeds the format it **already** produces, no transcoding
between framings:

- **NVENC**: Annex-B with `repeatSPSPPS` on IDRs -> `Avc3`. Passthrough.
- **VAAPI (cros-codecs)**: emits an Annex-B elementary stream with in-band
  SPS/PPS (VAAPI packed headers) -> `Avc3`. Passthrough. (Confirmed from source,
  see cros-codecs notes below.)
- **VideoToolbox**: emits **AVCC** (length-prefixed) with SPS/PPS out-of-band in
  the `CMFormatDescription` -> maps **directly onto `Avc1`**. On the first
  keyframe we read SPS/PPS from the format description, build the avcC, and call
  `initialize()` with it; thereafter we pass the length-prefixed sample data
  straight through. **No AVCC->Annex-B conversion, no per-frame SPS/PPS
  splicing** -- this removes what I'd previously flagged as the bulk of the
  VideoToolbox work.

Consequence for wiring: the producer's `Mode` is backend-dependent (Avc1 for
VideoToolbox, Avc3 for the rest). Today `Producer::new` hardcodes `Avc3`
([`producer.rs:34`](src/encode/producer.rs)) and is created *before* the encoder
opens. Two options:

1. **Backend declares its mode.** Add `fn wire_mode(&self) -> Mode` and
   `fn description(&self) -> Option<Bytes>` (the avcC for Avc1) to the trait, and
   create the `Import` once the backend is open. The capture loop already defers
   the catalog rendition until the first encoded frame
   ([`producer.rs:163`](src/encode/producer.rs)), so this reorder is natural.
2. **Normalize VideoToolbox to Annex-B** in its glue and keep everything `Avc3`.
   Simpler wiring, but adds the AVCC->Annex-B + SPS/PPS work back. Prefer (1).

### cros-codecs (VAAPI) notes

From reading the 0.0.6 source (`src/encoder/`, `examples/ccenc/`):

- **It works and fits our framing.** `EncoderConfig { resolution, profile
  (default Baseline), level, pred_structure: LowDelay { .. } (no B-frames -- good
  for real-time), initial_tunings }`. `Tunings { framerate, bitrate }` via
  `RateControl::{ConstantBitrate, ConstantQuality}`, changeable mid-stream with
  `tune()`. Output is `CodedBitstreamBuffer { bitstream: Vec<u8>, .. }`, an
  **Annex-B elementary stream with in-band SPS/PPS** -> `Avc3` passthrough.
  Input is **NV12**. The `simple_encode_loop` / `poll()` API matches our
  send-frame/drain shape.
- **The catch: it's ChromeOS-shaped, built around GBM / DMA-buf frames.** The
  ergonomic high-level path (`c2_wrapper::C2VaapiEncoder`) consumes `VideoFrame`s
  backed by a `GbmDevice` and `GenericDmaVideoFrame` (DMA-buf). Feeding a
  malloc'd NV12 buffer from a webcam means either allocating a GBM frame and
  `memcpy`-ing into its mapped planes (what `ccenc` does), or driving the
  lower-level `StatelessEncoder` and uploading to VA surfaces ourselves. Both are
  more plumbing than ffmpeg's "hand me a byte slice," and the stateless API is
  heavily generic (`StatelessEncoder<Codec, Backend, ..>`).
- **Runtime deps:** `libva` + a usable VA driver, and (for the GBM path) a DRM
  render node (`/dev/dri/renderD128`). Fine on a desktop/server with a GPU;
  worth noting for headless/container deploys.

Implication for sequencing: VAAPI is the **highest-effort, highest-risk**
backend. openh264 already covers non-NVIDIA Linux functionally (just not
GPU-accelerated), so VAAPI can land **last** and stay behind its feature flag
without blocking the ffmpeg removal.

### Pixel format pipeline

All three encoders want **NV12** (VideoToolbox also takes I420; NVENC/VAAPI
prefer NV12). The trait input is `Nv12`. The `Encoder` owns the converter that
turns whatever capture hands us into NV12, replacing ffmpeg's `swscale`:

- Converter crate: [`dcv-color-primitives`](https://crates.io/crates/dcv-color-primitives)
  (AWS, SIMD, maintained) or [`yuv`](https://crates.io/crates/yuv) (libyuv-like).
  Lean `dcv-color-primitives` for maintenance pedigree.
- `encode_rgba` (the bring-your-own-frames path) becomes RGBA -> NV12 via the
  same converter; the row-stride care in
  [`rgba_frame`](src/encode/encoder.rs) carries over.

### Capture replacement

Replace `libavdevice` with [`nokhwa`](https://crates.io/crates/nokhwa)
(avfoundation / v4l2 / msmf). `Camera::open/read/width/height/framerate` keep
their signatures so [`capture_loop`](src/encode/producer.rs) is untouched.

Wrinkles to handle:

- Linux UVC cameras commonly deliver **YUYV (4:2:2)** or **MJPEG** only. nokhwa
  decodes MJPEG behind a feature flag; enable it. Both get converted to NV12 by
  the same converter.
- macOS AVFoundation gives NV12/YUYV directly.
- Device-string semantics (index vs `/dev/videoN` vs name) differ from ffmpeg;
  re-document the `--camera` flag accordingly and keep the `Config` shape.

## Cargo features and target defaults

```toml
[features]
default = []                       # capture pulled in by moq-cli's `capture` feature
software = ["dep:openh264"]        # opt-in software fallback, all targets

[target.'cfg(target_os = "macos")'.dependencies]
objc2-video-toolbox = "..."
objc2-core-media = "..."
objc2-core-video = "..."

[target.'cfg(target_os = "linux")'.dependencies]
# Hardware encoders are always-on for Linux (cfg-gated, no feature). Both
# dlopen their drivers at runtime, so they link on a GPU-less builder.
moq-nvenc = { path = "../moq-nvenc" }  # in-tree fork, dlopen-only
moq-vaapi = "0.0.2"                 # standalone; vendored cros-libva + cros-codecs

[dependencies]
openh264 = "..."   # always-on software fallback
```

Hardware encoders are always-on (VideoToolbox on macOS, Media Foundation on
Windows, NVENC + VAAPI on Linux); the runtime fallback chain skips whichever
driver is absent. None is a build-time hard dep on the driver, so the binary
still builds and runs on a box with no GPU. openh264 is always compiled in as
the software fallback, so a GPU-less box still encodes (it's also what moq-boy
uses for its tiny 160x144 frames, which hardware encoders may reject).

### Selection / fallback (`Kind` mapping)

`open_backend(kind, config)` builds an ordered candidate list and returns the
first that opens, mirroring today's `open_encoder` loop:

- `Auto`   -> \[videotoolbox | nvenc | vaapi] (cfg-filtered), then openh264.
- `Hardware` -> hardware-only; `NoEncoder` if none opens.
- `Software` -> openh264 only.
- `Named(id)` -> that backend only.

A backend "fails to open" (driver missing, no device) the same way an ffmpeg
`find_by_name` miss does today, so the existing fallback semantics and
`Error::NoEncoder(tried)` carry over unchanged.

## Packaging payoff

- **macOS**: VideoToolbox links only system frameworks. One brew bottle per
  arch, no `Depends`.
- **Linux**: one binary that

  - `dlopen`s `libnvidia-encode` if an NVIDIA driver is present (no build dep),
  - `dlopen`s `libva` for Intel/AMD (no build dep on libva-dev) — *intended*;
    moq-vaapi 0.0.2 currently links libva instead, so this isn't realized yet
    (see #1837),
  - falls back to the always-compiled-in openh264 when no GPU encoder is usable.

  That single artifact runs across Ubuntu 20.04 -> 24.04, Debian, Fedora, etc.,
  which is the whole reason for the change.

## Migration phases

1. **Trait refactor, ffmpeg still under it.** Introduce `Backend` + `open_backend`,
   move the current ffmpeg encoder behind an `ffmpeg` backend impl. No behavior
   change; pure restructure with the existing tests green. (Keeps the diff
   reviewable and proves the seam.)
2. **openh264 backend** + the NV12 converter, with unit tests (gray-frame ->
   Annex-B, the existing assertions still apply). Backend emits `Avc3`.
3. **VideoToolbox backend** on macOS via `Mode::Avc1` (read SPS/PPS from the
   format description once, pass length-prefixed samples through; test on real
   hardware).
4. **NVENC backend** on Linux behind its feature.
5. **Capture swap to nokhwa; drop `ffmpeg-next`.** Delete the ffmpeg capture +
   scaler, remove the dep from `Cargo.toml`, update \[CLAUDE.md cross-package
   notes], `doc/bin/cli.md`, the `capture` feature wiring in `moq-cli`, and the
   packaging recipes. After this ffmpeg is gone and the binary is GPU-accelerated
   on macOS/NVIDIA, software (openh264) elsewhere.
6. **VAAPI backend** (cros-codecs) last, behind its feature -- the GBM/DMA-buf
   plumbing is isolated and non-blocking once openh264 covers the fallback.

This work (including the capture swap and ffmpeg removal) ships to `dev`, since
it's a breaking change to `moq-video`'s public API and a dependency overhaul.
It reaches `main` on the next `dev` -> `main` merge.

## Risks / open questions

- **cros-codecs is the biggest integration cost.** Capability is confirmed
  (H.264 VAAPI encoder in published 0.0.6, NV12 in, Annex-B out), but its
  GBM/DMA-buf frame substrate + heavily-generic stateless API make it the
  hardest backend to wire to CPU webcam frames. Pre-1.0, thin docs. Pin exact,
  wrap tight, land it last behind its feature flag.
- **VideoToolbox glue is smaller than first thought.** Mapping its native AVCC +
  out-of-band SPS/PPS onto `Mode::Avc1` removes the AVCC->Annex-B conversion.
  Remaining work: session lifecycle, the property dict, callback handling, and
  reading SPS/PPS from the `CMFormatDescription` to build the avcC once. Still
  the main *new* code, but well-trodden; budget for on-device debugging.
- **nokhwa format coverage**: verify MJPEG + YUYV paths on a real Linux UVC cam;
  confirm on-demand open/close (LED off when unwatched) still works, since the
  gate logic in `capture_loop` depends on it.
- **Quality/latency parity**: openh264 < libx264 quality; verify hardware presets
  match today's `realtime=1` / `zerolatency` low-latency behavior.
- **Coverage gaps vs today**: no Intel QSV-specific path (Intel goes through
  VAAPI), no Windows, no Raspberry Pi `v4l2m2m`. Note in docs; cros-codecs'
  stateful V4L2 encoder could cover the Pi later.

## As built (phases 2-5)

Where the implementation differs from the plan above:

- **Single wire format: Avc3 everywhere (not Avc1 for VideoToolbox).** Routing VT
  through `Avc1` would have needed the avcC up front, which breaks the
  advertise-track-before-camera-opens on-demand model. Instead every backend
  emits Annex-B and the producer stays `Avc3`. The VideoToolbox backend converts
  its native AVCC + out-of-band SPS/PPS to Annex-B in its output callback
  (length-prefix -> start code, SPS/PPS from the format description spliced in on
  IDRs). The `Backend` trait gained no `wire_mode`/`description`.
- **Boundary is I420, not NV12.** openh264 wants I420; VideoToolbox takes a planar
  `420YpCbCr8Planar` CVPixelBuffer; NVENC takes `IYUV`. So I420 needs no
  per-backend pixel conversion. `backend::Frame` is tightly-packed I420.
- **Converter is the `yuv` crate, not `dcv-color-primitives`.** dcv 0.7 has no
  RGBA -> I420 path (only BGRA/ARGB/BGR), and dcv 1.0 needs rustc 1.87 > our
  pinned 1.85. `yuv::rgba_to_yuv420` does it directly (BT.601, limited range).
- **No scaler.** The camera is opened first and the encoder is sized to its
  negotiated resolution, so capture frames already match the encoder; `encode_rgba`
  now requires input dims == encoder dims (it errors otherwise) instead of
  rescaling. This dropped swscale entirely.
- **Capture is per-platform native (nokhwa fully removed).** Each platform's
  `Camera::read` yields a `Frame` the encoder can take: macOS hands VideoToolbox a
  zero-copy `CVPixelBuffer` surface, Linux V4L2 and Windows Media Foundation hand
  the software/NVENC path a CPU `I420`. macOS AVFoundation/ScreenCaptureKit and
  Linux V4L2 (YUYV resampled, MJPEG via `zune-jpeg`) landed first; Windows uses an
  `IMFSourceReader` with its video processor enabled to coerce the camera's native
  format to NV12, which we deinterleave to I420 (`I420::from_nv12`). The device
  string is uniform: "bare integer = index, else path/name" (a friendly-name
  substring on Windows).
- **`Error` slimmed.** The ffmpeg-specific variants (`Ffmpeg`, `NoCaptureBackend`,
  `NoVideoStream`) and the `From<ffmpeg_next::Error>` impl are removed; capture and
  encode failures now flow through `Error::Codec(anyhow)`.

### Verified vs unverified

- **Verified on macOS** (real hardware, `just check`): openh264 and VideoToolbox
  encode synthetic frames; a VideoToolbox test asserts the AVCC -> Annex-B IDR
  carries SPS+PPS+slice. moq-cli `--features capture` and moq-boy still build.
- **NVENC compiles everywhere but its runtime is UNVERIFIED.** The `moq-nvenc`
  crate (nvidia-video-codec-sdk 0.4 fork, dlopen-only) is a workspace member and
  compile-checks on the dev Mac (nothing links; NVENC is only loaded on Linux).
  Actually encoding still needs a Linux+GPU (or CI) pass to confirm: (1) the flat
  input-buffer `write` matches NVENC's pitch (safe only for 64-aligned widths; we
  warn otherwise); (2) forced-IDR via `picture_type` with picture-type-decision
  enabled.
- **Windows Media Foundation capture is UNVERIFIED on hardware.** The `windows`
  0.62 FFI is fully type-checked against the `x86_64-pc-windows-msvc` target (the
  whole crate can't cross-compile from the dev Mac because openh264's vendored C++
  build needs MSVC, but a scratch crate confirms every Media Foundation call), and
  the COM/refcount lifecycle is reviewed. Still needs a real Windows + webcam run
  to confirm: (1) the source reader's video processor actually delivers NV12 for
  common cameras (MJPEG/YUY2 sources); (2) `IMF2DBuffer::ContiguousCopyTo` yields
  unpadded NV12 so the I420 deinterleave is correct; (3) the on-demand
  open/`source.Shutdown()` cycle releases the camera (LED off) and reopens cleanly.

### VAAPI reintroduced via discord/cros-codecs + NV12 surface upload

VAAPI was dropped in #1704 because `cros-codecs 0.0.6` (still the latest release)
caret-pins `cros-libva 0.0.12`, which does not compile against libva >= 2.23: the
newer headers add `seg_id_block_size` / `va_reserved8` to
`VAEncPictureParameterBufferVP9`, and `cros-libva 0.0.12`'s struct literal omits
them. `cros-libva 0.0.13` fixes it, but `cros-codecs 0.0.6`'s `^0.0.12` pin won't
accept it, and upstream cros-codecs is unmaintained (no release since June 2025).

The unblock is [discord/cros-codecs](https://github.com/discord/cros-codecs), an
actively-maintained fork (Discord ships it for Go Live) that bumps to cros-libva
0.0.13 *and* hardens the H.264 VAAPI encoder (packed SPS/PPS + slice headers,
`frame_num`, rate control). We consume it as a git dependency, no fork of our own:

- `cros-codecs = { git = discord/cros-codecs, branch = "discord-0.0.5", features = ["vaapi_dlopen"] }`.
- `vaapi_dlopen` pulls the cros-libva `dlopen` feature, which lives on
  discord/cros-libva's `discord-0.0.13` branch, so the root `[patch.crates-io]`
  points `cros-libva` there. Both git URLs are in `deny.toml`'s `allow-git`.

**dlopen, like NVENC.** `dlopen` makes cros-libva load libva at runtime (no
`cargo:rustc-link-lib`, no `DT_NEEDED libva.so.2`), so a `--features vaapi` binary
links on a libva-less builder and loads on a libva-less machine, falling back to
software (see `backend::open`). The build still needs libva *headers* for
cros-libva's bindgen, so `libva` is in the nix devShell.

> **Status (moq-vaapi 0.0.2): not yet realized.** The published `moq-vaapi` crate
> we depend on today *links* libva via `pkg_config` (its `build.rs` probes `libva`
> and `libva-drm`), so the binary carries `NEEDED libva.so.2` / `libva-drm.so.2`
> and needs libva at both build and run time; a libva-less host fails to load
> rather than falling back. Restoring the `dlopen` path in `moq-vaapi` (so this
> section holds) is tracked in #1837.

**Input is an NV12 surface upload, not zero-copy dmabuf.** The encoder wants an
NV12 VA surface, but UVC webcams deliver YUYV/MJPEG (decoded to CPU I420); they
rarely expose NV12 to import zero-copy. So `backend/vaapi.rs` drives
`new_native_vaapi` with a `VaSurfacePool`: each frame uploads I420 into a pooled
surface as NV12 (`libva::Image`, honoring plane pitches) and encodes the surface.
This works with the existing CPU V4L2 capture, no new capture code.

Follow-up (not in this PR): the **zero-copy dmabuf path** for the rare NV12-capable
V4L2 source. Re-add `Frame::DmaBuf`, a V4L2 `VIDIOC_EXPBUF` capture (the `v4l`
crate exposes the raw ioctl but no dmabuf stream), and a `requires_dmabuf` capture
coupling, then import the dmabuf into a VA surface (`MemoryType::DrmPrime2`).

**NOT YET VALIDATED ON HARDWARE.** Compiles on Linux with libva headers; written
against discord/cros-codecs `discord-0.0.5` with type/field names checked against
source. Needs a Linux + Intel/AMD GPU to confirm: (1) the `low_power` entrypoint
(recent Intel iHD often requires the low-power encode entrypoint, AMD the full
one; we request full and let `Kind::Auto` fall back); (2) the NV12 upload
pitch/offset handling round-trips; (3) `cargo deny` accepts the new transitive
licenses (drm, drm-fourcc, etc.) once the vaapi graph resolves.

### NVENC ships via dlopen (no driver dependency at build or load)

For the "single binary reaches the GPU at runtime" goal, NVENC must not hard-link
the driver. The stock `nvidia-video-codec-sdk` emits
`cargo:rustc-link-lib=nvidia-encode` / `nvcuvid`, which would make an
`--features nvenc` binary (a) impossible to link on a GPU-less builder and (b)
fail to even load on a machine without the NVIDIA driver (`DT_NEEDED
libnvidia-encode.so.1`), before `backend::open`'s software fallback could run.

So `nvenc` dlopens everything at runtime, like `cudarc` does for CUDA:

- `cudarc/fallback-dynamic-loading` dlopens `libcuda`; `cudarc/cuda-12020` pins the
  CUDA API version so the build needs no CUDA toolkit.
- `moq-nvenc` dlopens `libnvidia-encode`. Our in-tree fork (`rs/moq-nvenc`) is
  trimmed to this one mode: the SDK already routes every call through a function
  table built from two entry points (`NvEncodeAPICreateInstance` /
  `GetMaxSupportedVersion`), and the fork resolves those two via `dlopen` instead
  of linking them (there is no `build.rs`, so nothing links).

Result, verified on a GPU-less Linux box: `--features nvenc` builds, links, and the
test suite runs and passes (NVENC unavailable -> falls back to openh264), and the
binary has no `libnvidia-encode` / `libcuda` `DT_NEEDED`. So one portable `moq-cli`
can carry NVENC and use it only where the driver is present.

### Follow-ups

- CI builds + tests NVENC normally on Linux (`cargo {check,test} -p moq-video --all-features`); the dlopen feature means no GPU/driver and no special flags are
  needed. moq-video stays excluded from the *workspace* `--all-features` runs only
  because its SDK crate has no macOS bindings (see `rs/justfile`'s `ci` recipe).
- Still needed on real hardware: NVENC encode validation on a Linux+GPU box (pitch
  alignment, forced-IDR); only synthetic-frame software encode is tested here.
- Live camera run (capture needs camera/screen permission, which a headless or
  agent-spawned process can't obtain; run `moq-cli ... capture` from a user
  terminal). On Windows this is also where the Media Foundation path gets its
  first real exercise (see the unverified note above).
- Consider reusing NVENC input/output buffers across frames (currently allocated
  per frame to sidestep the self-referential Session borrow).

## Beyond H.264: HEVC (added later)

The "H.264 only" scope above was the ffmpeg-removal project. H.265 encode landed
afterward on top of the same `Backend` seam:

- `Encoder`/`Config`/`Options` gained a `Codec` field (`H264` / `H265`);
  `Producer::new` takes the codec and routes packets to the matching
  `moq_mux::codec` importer (`.avc3` / `.hev1`). The mux + `hang` catalog already
  supported both, so only `moq-video` needed work.
- Backends advertise the codecs they emit and `backend::open` filters by the
  requested codec before applying `Kind`. **H.265**: hardware-only (no software
  encoder). On macOS, VideoToolbox (one extra codec type + profile, an
  HVCC->Annex-B path reusing the H.264 conversion, and HEVC NAL/IRAP parsing). On
  Windows, the Media Foundation MFT, which natively emits Annex-B with inline
  VPS/SPS/PPS like its H.264 output, so it only needs the HEVC output subtype +
  Main profile, no rewrite. The MFT is vendor-agnostic (NVIDIA/Intel/AMD).
- Verified on macOS (`just check`): VideoToolbox HEVC emits a self-contained
  VPS+SPS+PPS+IDR Annex-B keyframe, and the full encode -> split -> import ->
  catalog round-trip registers the right rendition for each codec.
- Verified on Windows (RTX 3070 Ti, live camera): Media Foundation HEVC publishes
  a `.hev1` rendition; the round-trip into Matroska decodes as Main-profile HEVC.
- Linux NVENC HEVC followed: the NVENC backend selects its codec GUID from
  `Codec`, so H.265 reuses the H.264 preset / GOP / rate-control path and emits
  Annex-B with inline VPS/SPS/PPS. Unvalidated on hardware like NVENC H.264.
- Follow-ups: Linux VAAPI HEVC, and a live camera run per platform.

AV1 is intentionally left out: there is no hardware AV1 encoder available to us
(none on macOS; NVENC/VAAPI AV1 are Linux-only and not yet wired), and software
AV1 (rav1e) is too slow for real-time capture. AV1 returns whenever a hardware
backend lands. The `Codec` enum is `#[non_exhaustive]`, so adding it back is not
a breaking change.
