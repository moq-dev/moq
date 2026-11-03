# [M] Hardware encoder rows gate the nightly

## Goal

The nightly scores NVENC (Ampere on `nixos`, Turing on `nixos2`) and VA-API
(Coffee Lake on `nixos2`, Arrow Lake and RADV on the desktop) on the same
clips and rungs as OpenH264. Every row has its own floor and pop ceiling, so
a driver or backend regression on any card goes red. The report puts the
backends side by side for each clip and rung, and `just test video-quality`
prints the same table locally on any GPU host.

## Plan

Decisions are recorded in the [epic](/quest/m2/video-quality/README.md):
every row gets floors, and an offline host fails its job. Local runs write
their table to `.tmp/` and upload nothing.

- Key a row by host, backend, clip, and rung, and record the device and
  driver version in the report rather than in the key. A driver update that
  moves a score then fails loudly and gets re-measured on purpose, instead
  of starting a new row that has no floor.
- Hardware output is not bit-exact across runs. Measure each row's spread
  over several nightlies before setting its floor.
- Select the backend explicitly per row. moq-transcode's default prefers
  hardware, so a missing driver must fail the row, not fall back to
  OpenH264.
- When [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) lands, its
  backend joins the desktop's rows.

Public API: none. Wire: none.

## Required

- [Harness](/quest/m2/video-quality/harness.md) - the scorer and row format
- [GPU CI](/quest/m2/gpu-ci.md) - `nixos`'s runner gains its GPU
- [GPU runners](/quest/m2/video-quality/gpu-runners.md) - `nixos2` and the desktop host runners
