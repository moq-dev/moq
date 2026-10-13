# [S] A release carries multi-vendor GPU input

## Goal

The next breaking release carries the neutral `Surface::Vulkan`, Vulkan
Video encode on AMD, and VA-API import of external Vulkan images on Intel.
Both vendor proofs validate the input surface before publication.

## Plan

Decided 2026-10-13: ship these changes in the next main-to-release cut.
GPU capacity and health reporting is additive and remains separate. This is
a publication condition; the implementation and vendor proofs gate m1.

## Required

- [Vulkan Video encode on AMD](/quest/m1/vulkan-encode.md) - AMD proof
- [VA-API Vulkan import](/quest/m1/vaapi-vulkan-import.md) - Intel proof

## Related

- [GPU health](/quest/m2/gpu-health.md) - later reporting, not a surface release gate
