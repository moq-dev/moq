# [M] VA-API on radeonsi holds the cap when a still scene turns busy

## Goal

moq-video's VA-API encoder on Mesa radeonsi keeps each second within about
the rendition's cap when a still scene turns busy, for every codec it drives
there. A test feeds that switch and measures the worst one-second window.

## Plan

Found by [moq-dev/moq#9193](https://github.com/moq-dev/moq/pull/9193), which
measured FFmpeg 9.0.2's `{h264,hevc,av1}_vaapi` configured the way OBS 32.2.2
configures it (cap 3 Mbps, `rc_initial_buffer_occupancy` at the max rate, no
B-frames, 2 s GOP) on Big Buck Bunny 720p30 clips. Its CBR side note: "When a
still scene turns busy, the worst 1 s window reaches 14.5 Mbps (VCN 4) to 30
Mbps (VCN 5 HEVC) against 3 Mbps, and HEVC CBR on VCN 5 averages 9.3 Mbps on
noise." Selected CBR rows (mean / worst 1 s, kbps): radeonsi 610M H.264
noise 3297 / 7006, RX 9070 H.264 noise 3032 / 7351, RX 9070 AV1 noise
5983 / 9893; iHD H.264 2363 / 3680.

Decided 2026-11-19 (maintainer, /quest-complete and /quest-plan):

- ✅ Work around it in moq (rejected: report it to Mesa, which the
  maintainer would have to write since Mesa's policy bars AI-written GitLab
  text; drop it). No Mesa issue tracks it as of 2026-11-19.
- ✅ moq-video only (rejected: moq-video and OBS; OBS only). Reason: "leave
  OBS to do its own encoding in general, while moq-video owns this low level
  tweaking."

Reproduce first: #9193's numbers may not carry over. moq-video's path
(`rs/moq-vaapi/src/encode.rs`) differs from FFmpeg's: CBR on radeonsi
(`RateControl::pick`, `:152-177`), an HRD of 3 frames of the bitrate
(`HRD_FRAMES`, `:89`, `hrd_size` at `:736-738`) instead of about 1 s, a QP
floor of 24 (`MIN_QP`, `:84`), and bit stuffing off (`RcFlags`, `:591`).
`quality_under_the_bitrate` (`rs/moq-video/src/encode/backend/vaapi.rs:573-617`)
held radeonsi within 2%, but its `spend` helper (`backend/mod.rs:583-631`)
encodes calm or busy content separately, never a switch, and notes that the
RX 9070 on Mesa 26.0.8 encodes no H.265 that narrow. No still-to-busy
harness exists: add a `spend` case that switches from a still frame to noise
mid-run and reports the worst one-second window.

If it overshoots, fix it in moq-vaapi or moq-video. Recommended, in order:
the unused `EncMiscParameterBufferMaxFrameSize`
(`rs/moq-vaapi/src/buffer/enc_misc.rs:177`) sized from the HRD, then a size
guard above the backend in the shape of `js/publish/src/video/quantizer.ts`
(a leaky bucket of budget that skips frames rather than overshoot). Hosts:
the desktop (RX 9070, VCN 5) and fedora (Radeon 610M iGPU, VCN 4).

Public API: none expected. Wire: none.

## Hosts

- linux - radeonsi VA-API needs an AMD GPU on Linux
