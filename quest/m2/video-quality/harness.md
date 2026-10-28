# [L] Video quality harness

## Goal

`just test video-quality` encodes each corpus clip, scores every output frame
with VMAF against its source, and writes a per-frame and per-GoP report. It
fails when a row falls below its VMAF floor or a GoP boundary exceeds its pop
ceiling. A nightly job on a hosted runner gates the OpenH264 rows, and their
scores reach the benchmark table.

## Plan

Decisions are recorded in the [epic](/quest/m2/video-quality/README.md).

- Layout: `test/video-quality/`, mirroring `test/audio-quality/`, with
  per-row floors and ceilings in a budgets file measured from nightlies by the
  same rule, and an entry in `test/README.md`.
- Scorer: the `vmaf` CLI from nixpkgs `libvmaf` (3.0.0 as of 2026-10-28). It
  reads two Y4M files and writes per-frame JSON, so it does not need
  `ffmpeg-full` in the shell. PSNR and SSIM are opt-in features
  (`--feature psnr --feature float_ssim`).
- Two layers, both encoding through moq-video so backend choice stays the
  one used in production:
  - Encoder rows: Y4M in, `moq_video::encode::Encoder` on an explicit
    backend, decoded back to Y4M.
  - Transcode rows: the clip is published as a high-bitrate source broadcast
    on an in-process origin, and each `moq_transcode` rung is subscribed and
    decoded. Each rung is scored against the source as decoded, so that
    transcode loss is separated from source loss. Bicubic-upscale the rung to
    source resolution before scoring, per VMAF convention.
- Decode with the software decoder so the scoring side does not vary by host.
- Report, per frame: VMAF, bytes, and frame type. Per GoP: mean and minimum
  VMAF, bytes, and both pop ratios as the epic defines them. Leave out IDRs
  that land on a source scene cut (a large source frame difference), since
  quality legitimately changes there. Such a GoP reports its boundary as
  excluded and does not count toward the pop gate.
- Include a static screen-content clip and a constant-color synthetic clip,
  so the pop metric's floored divisor is exercised, both clean and with an
  injected boundary step.
- Publish per-frame plots as a nightly artifact, and feed per-row mean VMAF,
  minimum VMAF, and worst pop ratio into the benchmark history format through
  `just gh benchmark-report`, as the other nightly sources do.
- Corpus manifest: synthetic clips generated at run time (cheap, always
  available), open clips such as Blender open movies (CC-BY) with each license
  checked before listing, and private clips from the
  [bucket](/quest/m2/video-quality/corpus-bucket.md). Cover screen content,
  talking head, high motion, and fades. Keep clips short: nightly CPU time
  is a budget.
- A run selects clips by name. A selected clip that cannot be fetched, or
  whose sha256 differs from the manifest, fails the run.
- OpenH264 is deterministic, so its floors can sit close to the measured
  value. Set no fixed acceptance number up front; measure, then record.

Public API: none. Wire: none.

## Required

- [Corpus bucket](/quest/m2/video-quality/corpus-bucket.md) - where open and private clips live

## Related

- [Audio ViSQOL](/quest/m2/audio-visqol.md) - the same floor rule for audio
- [Benchmark sources](/quest/m2/bench-sources.md) - the history format the scores join
