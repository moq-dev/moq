# [M] A vendor-free CARLA bridge

## Goal

`demo/carla/bridge` holds no vendor code. It hands moq-video one
`Surface::Vulkan` per capture with `Kind::Auto`, and moq-video picks the
import and encoder: NVENC on NVIDIA, Vulkan Video on AMD. The bridge names no
backend and no vendor ID. It encodes H.264 or H.265, and the worker publishes
whichever it is. `just carla bridge-gpu` runs the stand-in test on an AMD and
an NVIDIA host.

This is the end-to-end test that moq-video's API carries a GPU frame from an
external producer to any vendor's encoder without a CPU round trip. CARLA
itself stays on NVIDIA; the stand-in proves the bridge elsewhere.

## Plan

moq-video owns the surface, the backend selection, the Vulkan Video backend,
and its hardware tests. This quest only removes what the bridge still pins.

Already done: the bridge passes the plugin's device and driver UUID as
`encode::Config::input`, imports each slot with the exporter's
`memoryTypeIndex` and allocation size, recycles slots on moq-video's
completion, and takes keyframes from `Encoded::keyframe`. Its parameter-set
probe opens the camera's own backend with a CPU picture.

- Selection: switch `stream.rs` `config` from `Kind::Named("nvenc")` to
  `Kind::Auto`. A device no backend imports must be refused, never sent to
  openh264; the workspace unifies `openh264` on, so check that refusal in a
  test.
- Probe: a CPU picture only works on a backend that also takes CPU input.
  Probe with the camera's surface type, or take parameter sets from the first
  keyframe instead, whichever backend picks.
- Codec (H.264 and H.265): the probe's SPS/PPS filter becomes codec-generic.
  - The worker names each rendition's codec in its request, and
    `Event::Config` echoes it.
  - Both sides reject trailing bytes and the worker requires an exact hello
    `VERSION`, so defaults cannot make this compatible. Recommended: bump
    `VERSION`, so a mismatched bridge and worker fail at hello naming both
    versions; test the refusal both ways. Open, if mixed versions must run:
    negotiate at hello and keep the v1 layout for H.264.
  - `sim/moq_carla_sim/bridge.py` publishes `AVC3` or `HEV1` instead of
    hardcoding `AVC3`.
- Scaling: one render still feeds the 426x240 and 1280x720 renditions,
  scaled on the GPU. GPU-side copies are allowed; a CPU round trip is not.
- Test: the stand-in (`tests/standin/mod.rs`) stops requiring vendor
  `0x10de` and renders on the GPU `gpu.sh` found, by PCI vendor.
  - `bridge/gpu.sh` gains an AMD branch that runs the test with the host's
    RADV ICD visible to the Nix Vulkan loader.
  - An Intel-only host still skips: the plugin exports `OPAQUE_FD`, which
    VA-API cannot take.
  - Run H.264 and H.265 on AMD, and the NVIDIA branch on a CARLA host.
- Docs: `demo/carla/README.md` and the crate description stop saying NVENC.

## Required

- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - the AMD encoder this exercises

## Related

- [GPU CI](/quest/m2/gpu-ci.md) - runs the NVIDIA half nightly
