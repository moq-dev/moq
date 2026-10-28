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

Non-goals: per-viewer adaptation and live bitrate control, which belong to the
[ladder](/quest/m3/ladder/README.md); browser playback quality, which belongs to
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
  ceiling measured from nightlies, the way [ViSQOL](/quest/m2/audio-visqol.md)
  keeps `budgets.json`. Scores also feed the [benchmark
  table](/quest/m2/bench-sources.md) for trends (✅ "Floors + trend" over
  "Trend only" and "Golden diff", which is brittle across drivers).
- Milestone m2, ranked right after release execution (✅ "Near the top"
  over "Next to GPU CI" and "Next to audio ViSQOL"). Rate control under
  on-demand encoding is unmeasured, and moq-boy is visibly bad.
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

## Required

- [Harness](/quest/m2/video-quality/harness.md) - `just test video-quality` scores OpenH264 rows with per-frame VMAF and boundary pops against floors, nightly
- [On-demand pops](/quest/m2/video-quality/on-demand.md) - cold starts, resubscribes, and fetch-encoded groups spliced with live ones stay under the pop ceiling
- [moq-boy quality](/quest/m2/video-quality/moq-boy.md) - moq-boy's encode is measured on generated clips and fixed
- [Corpus buckets](/quest/m2/video-quality/corpus-bucket.md) - R2 serves open clips publicly and private clips, from a separate bucket, to a read-only CI secret
- [GPU runners](/quest/m2/video-quality/gpu-runners.md) - `nixos2` and the desktop host self-hosted runners for the nightly job
- [Hardware rows](/quest/m2/video-quality/hardware.md) - NVENC and VA-API rows from three GPU hosts gate the nightly and compare side by side
