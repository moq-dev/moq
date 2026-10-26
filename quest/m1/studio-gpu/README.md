# Studio on the GPU

## Goal

`moq-studio`'s pictures stay on the GPU from source to encoder on the
backends proven before the cut: sources reach the compositor as textures on
the shared device, and the program reaches the encoder as a GPU surface. A
CPU copy remains only as a counted fallback, visible in the studio's Output
panel.

## Plan

Decided 2026-10-25 (maintainer): in m1, so moq-video's GPU encode and import
API changes land before the cut. Each piece lands in moq-video or moq-scene,
never in the app, so other consumers inherit it.

Scope is the API shape plus the backends provable now. AMD Vulkan Video
encode, Intel VA-API on an external Vulkan image, and Direct3D11 render
import stay in m2 as Related follow-ups; until they land, those backends take
the counted CPU path. Nothing here requires m2 or m3 work, including the
[macOS and Windows runs](/quest/m3/studio/app.md): moq-video and moq-scene
test each platform's path on that platform's CI.

## Required

- [Zero-copy source import](/quest/m1/studio-gpu/import.md) - captured and decoded frames reach the compositor as GPU textures, with uploads counted

## Related

- [Studio](/quest/m3/studio/README.md) - the app these quests serve
