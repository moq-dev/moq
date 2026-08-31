# [XL] Merge iroh-live's native media stack into moq-video/moq-audio (upstreaming plan)

## Goal

Implement and verify the behavior tracked in [#2481](https://github.com/moq-dev/moq/issues/2481)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

n0-computer is winding down maintenance of [iroh-live](https://github.com/n0-computer/iroh-live) and has offered its native media stack upstream. Their side produced a detailed upstreaming plan ([overview](https://github.com/n0-computer/iroh-live/blob/plan-upstream/plans/upstream/overview.md), [comparison matrix](https://github.com/n0-computer/iroh-live/blob/plan-upstream/plans/upstream/comparison.md), [zero-copy analysis](https://github.com/n0-computer/iroh-live/blob/plan-upstream/plans/upstream/zerocopy.md)). This issue is an independent audit of that plan against moq's API principles, and the tracking plan for the moq side. The goal is a merge that lands as moq-shaped primitives, not a copy/paste of a parallel stack.

Companion to #1837 (this plan lands several of its open boxes). The end state: iroh-live deletes its parallel media layer (`rusty-codecs`, `rusty-capture`, `moq-media` device/render code) and consumes `moq-video`/`moq-audio`; moq gains the capabilities below.

#### What we gain

Audited against their tree; the assets with no moq counterpart or a strictly weaker one:

- **The only decode-to-render GPU path in either codebase**: a renderer with zero-copy surface import on three platforms and two graphics APIs (Vulkan DMA-BUF import incl. an Intel Y\_TILED re-tile blit, EGL/GLES DMA-BUF import, Metal `CVMetalTextureCache` aliasing), with a per-path fallback to CPU I420 and a three-strike path disable. ~3.5k LOC of the hardest hardware-specific code in either tree. This is the back half of the zero-copy story #1837 describes (we have ingest, they have egress).
- **VAAPI, validated**: a DMA-BUF-importing H.264 encoder with VPP scale/convert, validated on Intel Meteor Lake (our `encode/backend/vaapi.rs` is a 111-line CPU-only adapter whose own header says it is not validated on hardware), plus a full VAAPI stateless decoder with PRIME export. We have no VAAPI decode at all.
- **V4L2 M2M encode/decode**: the stateful hardware codec path for Pi/embedded. We have nothing there.
- **PipeWire DMA-BUF capture**: our PipeWire screen capture is CPU-only; theirs negotiates DMA-BUF and feeds VAAPI without a download.
- **Full-duplex audio**: a playback sink (device output, mixing, declicker fades, device switching, restart-with-backoff) and acoustic echo cancellation via `sonora` (a pure-Rust WebRTC audio-processing port on crates.io). This resolves the two blockers in #2282: moq-audio has no playback path, and we believed no pure-Rust AEC existed. Tracked in #2478.
- **Opus control-surface deltas** (runtime bitrate, correct OpusHead pre-skip, FEC/DTX plumbing) and a **PCM codec** for latency isolation. The pre-skip item is a real RFC 7845 conformance bug on our side. Tracked in #2480.
- Their comparison also surfaced a realtime-thread bug in our audio capture (unbounded channel from the cpal callback). Tracked in #2479.

Everything else in their matrix resolves to "use moq's, delete theirs" (openh264, VideoToolbox encode, macOS/Windows capture, dispatch, Annex-B tooling, resampler, nokhwa/xcap fallbacks). Those are deletions on their side; nothing lands here.

#### Where the two plans independently agree

> **Update 2026-07-24**: #2467 merged and landed the surface-handle base with a different (better) shape than either sketch below: the internal frame enum is now the public `#[non_exhaustive]` `Surface` enum (`PixelBuffer` on macOS, `Texture` on Windows, `Cuda` on Linux, `I420` everywhere), `decode::Frame.surface` is a public field, and the exits are total consuming conversions (`Surface::into_i420()` always; `Surface::into_pixel_buffer()` on macOS, a retain for a GPU frame and an upload for a CPU one) rather than a partial `native() -> Option<_>` borrow. There is no separate `Native` vocabulary and no newtype veil: the objc2 coupling is opt-in-by-use and documented via version re-exports. VideoToolbox decode also stays GPU-resident (the macOS half of the retention item below). The checklists below are updated to the as-landed shape; new zero-copy work should extend `Surface` (new cfg-gated variants, new total conversions), not introduce a parallel handle enum.

Their proposed base API is close to what #1837 already asks for, which is a good sign it is the right shape:

- Their `Native` handle vocabulary is #1837's "public, platform-tagged surface handle" item. Landed in #2467 as the public `Surface` enum (see update above). Their refinement that still carries: mint-on-access `DmaBuf::export()` (an fd per buffered frame would exhaust the descriptor table) and a `dmabuf` cargo feature enabled by `vaapi`/`pipewire`, for the future `Surface::DmaBuf` variant.
- Their `decode::Frame::native() -> Option<Native>` is #1837's "get the GPU handle out"; superseded by the public `decode::Frame.surface` field + total conversions, with `into_i420()` as the universal CPU fallback.
- Their VideoToolbox/Media Foundation decode-surface retention is #1837's "make decoded frames stay on the GPU first", verbatim.
- Their adaptation conventions (no ffmpeg, dlopen system libraries, crates.io only, `moq_net::Timestamp` at boundaries, hang catalog types, honest `set_bitrate`, `#[non_exhaustive]` configs) are our own house rules reflected back. Nothing to negotiate.

One genuinely new base item we accept: **timestamps through encode**. `Backend::encode` gains a `timestamp` argument and returns `Vec<Packet>` where `Packet { payload: Bytes, timestamp: Timestamp }` (`#[non_exhaustive]`). Today the capture loop stamps every returned AU with the submission-time clock, which is only correct for zero-frame-delay backends; a queued M2M device (V4L2, MediaCodec) drains frame N-k while you submit frame N. This also makes encode symmetric with decode (`Decoded` already carries a per-picture timestamp) and lets `finish()` drain a stamped tail, which per-group transcode wants anyway. All five current backends echo the input timestamp, so behavior is unchanged until a pipelined backend lands.

#### Where we deviate from their plan

These are the rulings that keep the merge from importing shapes we would have to live with:

1. **No backend registration API** (their B4: public `Backend` traits + `register_encoder`/`register_decoder` over a global `Mutex<Vec<Registration>>` of fn pointers). Rejected. The backend set is deliberately closed and `pub(crate)`; a public implementable trait freezes `Frame`/`Packet`/`Kind` interactions into semver surface for exactly one prospective consumer, and a process-global mutable registry is the callback-flavored indirection we keep out of this codebase. Android MediaCodec goes **in-tree** behind `cfg(target_os = "android")`, exactly like the objc2 and windows backend families. The NDK is a build-graph concern, not an API concern.
2. **The renderer is a `render` role module in moq-video, not a `moq-video-render` sibling crate.** #1837 already takes this position: `capture`/`encode`/`decode`/`render` are symmetric roles and the crate owns the platform-native layer on both ends. Heavy deps (`wgpu`, `ash`, `glow`, EGL) sit behind non-default features (`render`, `render-gles`), the same pattern as `nvenc`/`vaapi`/`pipewire`, so default and relay builds stay graphics-free. Port their importers (the hardware knowledge) but reshape the surface: a `render::Config` options bag, a small renderer type that consumes `decode::Frame` and yields a texture the caller presents, errors in moq-video's `Error`. The public `Native` accessor keeps the door open for anyone who wants an out-of-tree renderer instead.
3. **No `moq-egui` crate.** The renderer's texture-out seam (their `render_cached`) is the integration point for egui/bevy/dioxus; a published egui widget crate signs us up for egui's breaking-release cadence forever. An egui example can live under `demo/` or stay with iroh-live/community.
4. **No bespoke `publish_preencoded` API for libcamera.** A pre-encoded source composes with what exists: `encode::Producer::publish(Vec<Bytes>, Timestamp)` is already the bring-your-own-Annex-B path, and `moq_mux::codec::h264::{Split, Import}` already handle framing. The rpicam-vid subprocess source is useful (Pi Zero hardware encode) but shelling out is an application concern; open question whether it lands as a feature-gated moq-video source or in moq-cli. Wave 3 either way.
5. **PCM needs the spec, not just the code.** Their plan adds a PCM variant to the codec enum and the hang catalog but does not mention the draft. A catalog codec addition updates `hang` + `js/hang` + `draft-lcurley-moq-hang.md` in the same PR per the cross-package sync rule.
6. **AV1 software codecs stay out** (their own verdict too): rav1e is too slow for real-time and rav1d is a git-fork dep. Matches the existing #1837 constraint. NVDEC AV1 decode already covers the hardware side.
7. **X11 MIT-SHM capture is optional.** Portal/PipeWire covers modern desktops; take it last or not at all.

#### Waves

> **Update 2026-08-12**: The software and Windows work that was open in the July snapshot has landed: the renderer's wgpu/Metal/CPU half (#2552), playback (#2529), AEC (#2538, closing #2478), Media Foundation GPU decode retention (#2584), D3D11 GPU resize (#2601), and the crate documentation gate (#2567). The remaining campaign is now the Linux/embedded hardware chain. #2819 is the active leaf issue for the first producer-to-consumer slice: a safe `Surface::DmaBuf` contract, PipeWire buffer retention, and Vulkan import. Its hardware boxes stay open until an Intel/AMD Linux machine validates them.
>
> **Update 2026-07-27**: Wave 0 is done and Wave 2 is most of the way there. Landed since the last update: timestamps through encode (#2503), hardware resize for decoded pixel buffers (#2512, closing #2489), Opus pre-skip and encoder controls (#2492, closing #2480), the bounded realtime capture queue (#2487, closing #2479), and the PCM codec end to end including `js/hang` and the draft (#2493). The encode timestamp seam landed with a better shape than sketched: the timestamp rides on `Frame`, so the trait is `encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Encoded>, Error>` with no separate argument, and the output type is `Encoded { timestamp, payload }` rather than `Packet`. What is left on this list is now hardware-gated (DMA-BUF, VAAPI, PipeWire, V4L2, Media Foundation) plus the two pure-software items that unblock the native player #2272: the renderer and audio playback.

Wave 0, the base API (moq-video, one small series): **done except the two items whose consumers have not landed yet.**

- \[x] Public surface handles: landed in #2467 as the public `#[non_exhaustive]` `Surface` enum with `decode::Frame.surface` public, total `into_i420()` / `into_pixel_buffer()` conversions, and a validating public `I420::new`. (Replaced the `Native` vocabulary + `native()` accessor sketched originally; see the update note above.)
- \[ ] `Surface::DmaBuf` variant + a DmaBuf payload type with descriptor accessors and mint-on-access `export() -> OwnedFd`, behind a new `dmabuf` feature enabled by `vaapi`/`pipewire`. Prerequisite for VAAPI decode and PipeWire DMA-BUF capture. **Deliberately not landing ahead of them**: a `#[non_exhaustive]` variant with neither a producer nor a consumer is dead public API, so this lands in the same series as its first producer (VAAPI/PipeWire) and its first consumer (the Vulkan importer), hardware-validated together.
- \[ ] Egress accessors for the other GPU variants as their consumers land: public surface access on `d3d11::Texture` (Windows render/re-encode) and `cuda::Frame` (Linux), hardware-validated when added.
- \[x] Timestamps through encode: landed in #2503. The timestamp rides on `Frame` rather than a parallel argument, so the seam is `Backend::encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Encoded>, Error>` plus `finish() -> Result<Vec<Encoded>, Error>`, and the output type is `Encoded { timestamp, payload }` (`Packet` in the original sketch). The producer publishes per-access-unit timestamps, so a pipelined backend can drain frame N-k while frame N is submitted.
- \[ ] Additive `Error` variants as needed (`SurfaceExport`, `DmaBufImport`). Rolls in with the DMA-BUF and renderer work that needs them.

Wave 1, zero-copy egress (carries the campaign; nothing on their side is deleted until these land):

- \[x] VideoToolbox decode retains the GPU surface (macOS): done in #2467, hardware-validated (residency + zero-copy re-encode tests). The hardware-resize follow-up landed too: #2512 resizes decoded `CVPixelBuffer`s via `VTPixelTransferSession`, closing #2489 and the N-downloads-per-frame transcode fanout #2467 documented.
- \[x] Media Foundation decode retains its DXVA texture (Windows): #2584 keeps decoded frames GPU-resident and fixes group-boundary drain; #2601 adds D3D11 GPU resize. The renderer still has no Direct3D11 zero-copy import, so Windows presentation downloads today.
- \[ ] `render` module in moq-video: wgpu backend + NV12/I420 color-matrix-aware shader, Metal import (consuming `Surface::into_pixel_buffer()` / the `PixelBuffer` borrow), Vulkan DMA-BUF import (incl. VPP re-tile), GLES/EGL import, per-path fallback + disable. Non-default features. The renderer matches on `decode::Frame.surface` and falls back to `into_i420()`. **The wgpu, shader, Metal, and CPU half landed in #2552.** The Vulkan + `Surface::DmaBuf` slice is active in #2819; EGL follows after the Vulkan hardware gates.
- \[ ] VAAPI encode replaced with their validated DMA-BUF + VPP implementation; VAAPI decode added. Spine work in the external `moq-dev/vaapi` crate: restore dlopen (#1837), add the decode half, VPP wrapper, HEVC entrypoints.
- \[ ] PipeWire capture negotiates DMA-BUF (multi-fourcc) with CPU fallback.

Wave 2, audio + remaining codecs:

- \[x] moq-audio playback + AEC (#2478): playback landed in #2529; the post-mix render tap and pure-Rust `sonora` AEC landed in #2538, closing #2478. The dependency requires `sonora` 0.2 or newer because 0.1 could panic when the adaptive filter shrank.
- \[x] Bounded realtime capture channel: #2487, closing #2479.
- \[x] Opus pre-skip + runtime bitrate + FEC/DTX: #2492, closing #2480. The RFC 7845 pre-skip conformance bug is fixed.
- \[x] PCM codec in moq-audio + hang catalog variant + `draft-lcurley-moq-hang.md`: #2493, with `js/hang` and the draft's `audio-pcm` section in the same PR.
- \[ ] V4L2 M2M encode + decode backends. The encode timestamp seam they need now exists (#2503); the remaining blocker is embedded hardware to validate on.

Wave 3, conditional:

- \[ ] Android MediaCodec encode/decode in-tree (`cfg(target_os = "android")`), a `HardwareBuffer` variant on `Surface`. Weigh against moq-kit per #1837's mobile section before starting.
- \[ ] libcamera/Pi sources (raw + pre-encoded via the existing publish path).
- \[ ] X11 capture (optional).

Release/adoption gates:

- \[ ] Ordinary moq-video/moq-audio releases after each wave so iroh-live can pin and delete per their staged plan (they follow proof-before-deletion on their side; our side just needs published crates).
- \[x] `doc/` pages for moq-video and moq-audio: #2567 covers render, playback, AEC, backend selection, and the per-platform zero-copy matrix. `moq-ffi` exposure remains explicitly out of scope.
- \[x] Nix dev shell reaches ALSA's default device: #2529. It only searched its own store path, which has no `pipewire`/`pulse` PCM plugin, so opening the default device failed with ENXIO and device tests had to be pointed at a raw `hw:` node by hand. Affects the existing `capture` tests as much as the new playback ones.

#### Constraints carried over unchanged

- Zero-copy is never regressed: the decode-surface retention and the renderer land before iroh-live deletes its decoders, or the only decode-to-render path dies in the transition.
- Every hardware backend keeps the dlopen-and-degrade rule; hardware-gated tests ship with each backend (`#[ignore]` + reason where CI lacks the device).
- Licensing is compatible (both repos are MIT OR Apache-2.0); `sonora` and the cros-codecs-derived VAAPI code get a license check before their PRs.

Related: #1837, #2272 (the renderer + audio sink unblock the native player), #2282, #2321, #2147. Sub-issues: #2478, #2479, #2480, and #2489 are closed. Active Linux slice: #2819.

## Closes

- [#2481](https://github.com/moq-dev/moq/issues/2481) - close this issue when the quest finishes
