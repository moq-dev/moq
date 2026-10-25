# Studio on the GPU

## Goal

`moq-studio`'s pictures stay on the GPU from source to encoder: sources reach
the compositor as textures on the shared device, and the program reaches the
encoder as a GPU surface. A CPU copy remains only as a counted fallback for a
backend that cannot take a surface, visible in the studio's Output panel.

## Plan

Decided 2026-10-25 (maintainer): ranked at the top of m2, because the studio
drives moq-video improvements, mainly encoders taking a GPU surface and
zero-copy texture import. Each piece lands in moq-video or moq-scene, never
in the app, so other consumers inherit it.

Neither child requires the [macOS and Windows runs](/quest/m3/studio/app.md):
moq-video and moq-scene test each platform's path on that platform's CI, and
the studio-level assertions run on lavapipe. A backend whose moq-video
prerequisite has not landed keeps the counted CPU path until it does.

## Required

- [Zero-copy program encode](/quest/m2/studio-gpu/encode.md) - the program reaches the encoder as a GPU surface instead of a CPU readback
- [Zero-copy source import](/quest/m2/studio-gpu/import.md) - captured and decoded frames reach the compositor as GPU textures instead of a CPU upload

## Related

- [Studio](/quest/m3/studio/README.md) - the app these quests serve
