# Video quality

## Goal

A nightly harness runs a golden corpus through moq-video and moq-transcode
on every encoder backend we have hardware for. It scores each output frame
with VMAF against the source and turns the nightly red when a row's quality
drops or a GoP boundary visibly pops. Its rows include on-demand starts and
fetch-encoded groups, and moq-boy's encoding is fixed under it.

Today nothing measures encoded quality: the only check is a manual PSNR/SSIM
recipe in `rs/moq-video/examples/encode-presets.rs`. moq-boy's encoding looks
bad, and nobody knows how each backend or GPU compares on the same input.

Non-goals: per-viewer adaptation and live bitrate control, which nothing plans; browser playback quality, which belongs to
`just test media`.

## Plan

Decided in planning (2026-10-28):

- Outcome: harness, pop detection, per-backend rows, and the moq-boy fix
  (✅ "Yes, plus fix moq-boy" over "Yes" and "Narrower: pops only").
- Corpus: a public manifest in the repo (name, sha256, license, URL,
  geometry) with the blobs in R2. Open clips are public-read; private clips
  sit in a separate, non-public bucket that a CI secret reads. A selected clip that cannot be
  fetched fails the run instead of being skipped (✅ "Manifest + R2" over "Private
  repo (moq.pro)" and "Synthetic + open only").
- Gating: each row (host, backend, clip, rung) gets a VMAF floor and a pop
  ceiling measured from nightlies, the way [ViSQOL](/quest/m5/audio-visqol.md)
  keeps `budgets.json`. Scores also feed the [benchmark
  table](/quest/m4/bench-sources.md) for trends (✅ "Floors + trend" over
  "Trend only" and "Golden diff", which is brittle across drivers).
- Moved to lab 2026-11-12 (maintainer, /quest-audit): a test harness with no
  public API and no consumer waiting on it.
- Split (✅ "Epic, 4 quests" over "Epic, 3 quests" and "Single [XL]
  quest"): the harness, then on-demand pops, hardware rows, and moq-boy, each
  requiring the harness.
- Pop metric (✅ "VMAF step + flicker" over "VMAF step only" and "Also
  per-GoP bytes"). At each IDR the harness measures two things:
  - The VMAF step: the frame's VMAF against the mean of the frames before it.
  - The flicker: mean luma `|(enc[i] - enc[i-1]) - (src[i] - src[i-1])|`,
    the change the encoder added that the source did not have.

  Each is divided by the median of the same quantity at non-IDR frames, so a
  ceiling is a ratio that holds across content. Static content can make that
  median zero, so the divisor is floored at a just-noticeable change: a fixed
  VMAF step and a fixed mean luma difference, set once in the harness
  and applied to every row. A static clip then grades its boundary against
  that floor instead of dividing by zero. Per-GoP bytes are still reported,
  just not gated.
- On-demand pops: the on-demand quest measures them and fixes what it finds
  (✅ "Measure and fix" over "Measure only").
- Hardware: three self-hosted hosts run nightly and add rows. All rows have
  floors, and a host that is offline fails its job (✅ "Floors on every row"
  over "Report only" and "Floors, offline tolerated"). Local runs on other
  machines print a comparison table into `.tmp/` and upload nothing (✅ "No,
  local table" over "Yes, upload"). Hosts as checked on 2026-10-28:

  | Host | GPUs | Backends |
  | --- | --- | --- |
  | `nixos` | RTX 3070 Ti | NVENC (Ampere) |
  | `nixos2` | RTX 2070, Intel UHD 630 | NVENC (Turing), VA-API (Coffee Lake) |
  | maintainer desktop | Intel Arrow Lake iGPU, AMD RX 9070 | VA-API (Arrow Lake, RADV) |

- The moq-boy clip is generated, not stored. The emulator is deterministic,
  so the harness renders it from a public homebrew ROM on `rom.moq.dev` plus
  scripted input (✅ "Generate it" over "Store captures").
