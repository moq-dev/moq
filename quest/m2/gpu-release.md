# [S] A release carries multi-vendor GPU input

## Goal

A later release carries Vulkan Video encode on AMD and VA-API import of
external Vulkan images on Intel. Both vendor proofs validate their use of
the neutral `Surface::Vulkan` before multi-vendor support is published.

## Plan

Decided 2026-10-15: defer AMD/Intel implementation and publication together.
The next breaking cut makes no new AMD/Intel GPU-input support promise.
The existing surface on main is not evidence that those backends work.
GPU capacity and health reporting remains a separate additive follow-up.

## Required

- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - AMD proof
- [VA-API Vulkan import](/quest/m2/vaapi-vulkan-import.md) - Intel proof

## Related

- [GPU health](/quest/m2/gpu-health.md) - later reporting, not a surface release gate
