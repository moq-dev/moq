# [S] A release carries multi-vendor GPU input

## Goal

The `release` branch and crates.io carry the neutral `Surface::Vulkan`, the
Vulkan Video encoder, and VA-API import of an external Vulkan image, so a
consumer pinned to `release` (moq.pro is) can drop its vendor code.

## Plan

Release all three together: the VA-API and AMD proofs are what show the
surface is right, and shipping it before them risks a second breaking change.

The surface is a breaking `moq-video` change on `main`, and releases are
patch-only for now ([branch flip](/quest/m0/branch-flip.md)). Open, for the
maintainer at release time:

- Backport the moq-video change set onto `release` as a moq-video minor bump
  (recommended if no other crate's public API exposes the changed types).
- Wait for the next cut of `main` into `release`, which also brings its other
  breaking changes to every consumer.

## Required

- [One external GPU image for every encoder](/quest/m2/gpu-surface.md) - the neutral surface and auto-selection
- [Vulkan Video encode on AMD](/quest/m2/vulkan-encode.md) - the AMD encoder
- [VA-API encodes an external Vulkan image](/quest/m2/vaapi-vulkan-import.md) - the Intel proof
