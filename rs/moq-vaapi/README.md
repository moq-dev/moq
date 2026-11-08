# moq-vaapi

> **⚠️ AI GENERATED.** This crate was written by an AI agent (Claude), using the
> repositories below as references. How closely individual files track upstream
> varies, so treat the whole thing as derived work and review before relying on
> it. The encoder and the decoder have both been checked against ffmpeg's
> software codecs on Intel hardware (see below).

A small, self-contained **VA-API H.264 / H.265 hardware codec** for Linux (Intel / AMD),
derived from
[discord/cros-libva @ discord-0.0.13](https://github.com/discord/cros-libva/tree/discord-0.0.13)
and
[discord/cros-codecs @ discord-0.0.5](https://github.com/discord/cros-codecs/tree/discord-0.0.5)
(both BSD-3-Clause), themselves forks of Intel's
[cros-libva](https://github.com/intel/cros-libva) (ChromiumOS).

It gives `moq-video` a thin VA-API codec instead of a multi-backend framework.
It lived in [moq-dev/vaapi](https://github.com/moq-dev/vaapi) through 0.2.1.

## What it does

- **H.264 and H.265 encode** over VA-API: an Annex-B elementary stream out
  (packed parameter sets, low-latency IPPP, CBR rate control whose bitrate can
  change between frames, the color space labelled in the VUI). H.265 is Main
  profile with its VPS, SPS, PPS, and slice headers written here, its coding
  tools following what the driver reports, and its coded size following the
  driver's surface alignment, so the crop holds on radeonsi, which rewrites the
  SPS at that alignment. Three inputs:
  tightly-packed NV12 from the CPU, an NV12 DMA-BUF at the encoder's size
  encoded in place, and any other importable DMA-BUF (packed RGB, YUYV, NV12
  at another size or in another color space) converted and scaled into the
  encoder's surface on the GPU. NV12 and BGRX input are tested; the other
  formats go through the same video processor call untested.
  Checked on Intel Meteor Lake (iHD 26.1.5) by decoding with ffmpeg: luma
  above 35 dB PSNR against the source, and RGB converted to BT.601 and BT.709
  to within a few code values. These run as tests, for both codecs, wherever
  ffmpeg and a VA-API device are both present. Both codecs pass them on Intel
  Arrow Lake (iHD 26.3.5) and AMD Radeon RX 9070 (radeonsi, Mesa 26.2.5),
  where the stream also holds within a quarter of its bitrate from 2 Mbps down
  to 250 kbps.
- **DMA-BUF import and video post-processing.** `dmabuf` wraps a DRM PRIME
  buffer as a surface without copying it; `vpp` blits one surface into another
  through `VAEntrypointVideoProc`, scaling, converting, and re-tiling on the
  way. `Processor::resize` scales into NV12 surfaces it recycles per size, so a
  stream of resizes costs no allocation per frame.
- **H.264 and H.265 decode** over VA-API: one Annex-B access unit in,
  tightly-packed NV12 out, with in-stream parameter sets, a conformant DPB, and
  mid-stream resolution changes. H.264 covers reference marking, reordering,
  and frame\_num gaps; H.265 covers reference picture sets, long-term
  references, open-GOP CRA pictures, and dependent slice segments, and puts a
  picture out as soon as the SPS's reorder limit allows, so a stream without
  B-frames comes out with no delay. H.265 is the Main profile, plus Main 10 and
  Main Still Picture streams that hold 8-bit samples. Progressive 8-bit 4:2:0
  only, with right/bottom cropping supported and left/top cropping rejected.
  H.264 was verified bit-exact against ffmpeg's software decoder on Intel
  Meteor Lake (iHD 26.1.5) for constrained baseline, main with B-frames, high at
  720p, and a cropped non-macroblock-aligned size. Both codecs, coded by x264
  and x265 with B-frames at 320x240 and cropped at 852x478, run bit-exact as a
  test wherever ffmpeg and a VA-API device are both present, and pass on Intel
  Arrow Lake (iHD 26.1.2) and AMD Radeon RX 9070 (radeonsi, Mesa 26.0.8).
- **Checked-in bindings from pinned headers.** libva's headers are vendored
  into [`libva/`](./libva), and [`bindgen.sh`](./bindgen.sh) generates
  [`src/bindings/libva.rs`](./src/bindings/libva.rs) from them. The output is
  checked in, so building the crate needs **no libclang, no libva-dev, and no
  build script**, and the pinned version means a system libva bump can't drift
  the bindings. The file is portable: fixed-width C types, no layout tests, and
  the two VPP structs whose padding libva sizes by pointer width carry both
  layouts under a `cfg`.
- **Loads system libva at runtime on Linux.** The generated `Va` function table
  loads `libva` / `libva-drm` with `libloading`. Building needs no system libva,
  and a binary can start without it; opening a display reports a missing driver
  or library. The crate also compiles on macOS for development.

## Upstream

Last checked **2026-10-31**, starting from the Discord vendor baselines:

| Source | Vendored baseline | ChromeOS default branch checked |
| --- | --- | --- |
| cros-libva | [`discord-0.0.13`, `896a248e`](https://github.com/discord/cros-libva/commit/896a248e003caae7a7120812f7eaa423c93a920d) | [`59384456ac2ae78c0c3e5515f41ef1efd9b802cf`](https://github.com/chromeos/cros-libva/commit/59384456ac2ae78c0c3e5515f41ef1efd9b802cf) |
| cros-codecs | [`discord-0.0.5`, `9cdecb19`](https://github.com/discord/cros-codecs/commit/9cdecb192ee3682950d02bd80a7f106163531b76) | [`a050c6cde530e563197ee9e3e72ab01f48a0c41f`](https://github.com/chromeos/cros-codecs/commit/a050c6cde530e563197ee9e3e72ab01f48a0c41f) |

The forks diverge, so compare commits reachable from the checked head but not
the baseline, rather than replacing Discord's additions with the upstream tree.
This run checked all five cros-libva commits and the one cros-codecs commit:

| Commit | Disposition |
| --- | --- |
| cros-libva [`838808ce`](https://github.com/chromeos/cros-libva/commit/838808cebfeff64f5c3543c873dbc604857b3702), protected-content headers | Not needed: no protected-content consumer or Android build in this crate. |
| cros-libva [`89454d66`](https://github.com/chromeos/cros-libva/commit/89454d660411627a4a463116fdea00ac1ac888bc), explicit image lifetimes | Already present in `Image::create_from` and `Picture::{derive_image,create_image}`. |
| cros-libva [`9f3c0d92`](https://github.com/chromeos/cros-libva/commit/9f3c0d9264ec04b4fa792764178b879341ce33b1), packed-header bindings | Already present in the `bindgen` allowlist, `buffer.rs`, and the H.264 encoder. |
| cros-libva [`ed9c63b7`](https://github.com/chromeos/cros-libva/commit/ed9c63b743ce9490e1079a8b0c1a24b8e879324f), borrowed input buffers | Optional copy avoidance, not a correctness fix. Keep the existing owned-buffer API; no new public API or optimization without measurement. |
| cros-libva [`59384456`](https://github.com/chromeos/cros-libva/commit/59384456ac2ae78c0c3e5515f41ef1efd9b802cf), libva 2.23 VP9 fields | Build compatibility already covered by `..Default::default()` in the retained VP9 wrapper against pinned 2.23 headers. The codec drivers support only H.264 and H.265, so no VP9 segmentation control is needed. |
| cros-codecs [`a050c6cd`](https://github.com/chromeos/cros-codecs/commit/a050c6cde530e563197ee9e3e72ab01f48a0c41f), release log filtering | Ported: remove `log/release_max_level_debug`, which conflicts with a consumer selecting another release level. The crate now logs through `tracing`. |

GitHub's published security-advisory endpoints for
[chromeos/cros-libva](https://github.com/chromeos/cros-libva/security/advisories),
[chromeos/cros-codecs](https://github.com/chromeos/cros-codecs/security/advisories),
[discord/cros-libva](https://github.com/discord/cros-libva/security/advisories), and
[discord/cros-codecs](https://github.com/discord/cros-codecs/security/advisories)
all returned empty lists on this date. GitHub's global Rust advisory queries for
`cros-libva` and `cros-codecs` also returned none. This records the published
advisories checked, not a guarantee that no undisclosed vulnerabilities exist.

## Layout

- `src/`: libva bindings (`bindings`, `display`, `surface`, `buffer`, ...), the
  H.264 and H.265 bitstream layers (`bitstream_utils`, `codec::h264`,
  `codec::h265`), the H.265 header writer (`encode/h265.rs`), the thin encode
  and decode drivers (`encode`, `decode`), DMA-BUF import (`dmabuf`), video
  post-processing (`vpp`), and the color space both share (`color`).
- `libva/`: vendored libva headers.
- `libva-wrapper.h` and [`bindgen.sh`](./bindgen.sh): what generates
  `src/bindings/libva.rs`.

Regenerate the bindings with bindgen-cli on the PATH:

```sh
nix shell nixpkgs#rust-bindgen --command ./bindgen.sh         # from libva/
nix shell nixpkgs#rust-bindgen --command ./bindgen.sh 2.23.0  # refresh libva/ from that release first
```

## Licensing

BSD-3-Clause (see [`LICENSE`](./LICENSE)). Vendored upstreams keep their notices:
[`LICENSE.cros-codecs`](./LICENSE.cros-codecs) and the vendored libva headers
under [`LICENSE.libva`](./LICENSE.libva) (MIT).
