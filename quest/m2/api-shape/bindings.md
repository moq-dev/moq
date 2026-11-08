# [S] Fence MoqVideoSurface in the foreign bindings

## Goal

A new `MoqVideoSurface` variant (D3D11 and DMA-BUF surfaces, from
[the Windows](/quest/m5/obs-decode-windows.md) and
[Linux](/quest/m5/obs-decode-linux.md) decode quests) breaks no Kotlin,
Swift, Python, Dart, Go, or C++ caller. moq-ffi marks the enum
`#[non_exhaustive]`, which fences Rust and C only: UniFFI's generated
foreign enums stay exhaustive, and every wrapper aliases them today.

## Plan

Left from the 2026-11-07 API shape audit's bindings batch; see the
[epic](/quest/m2/api-shape/README.md). No other output-only UniFFI enum is
planned to grow.

## Waiting

- Maintainer: pick the fence. (a) Per-platform accessors on
  `MoqVideoDecodedFrame` (`pixel_buffer() -> Option<u64>`, later
  `d3d11_texture()`), dropping the enum, so each new surface is an additive
  method in every language (recommended: no wrapper code, and nothing to keep
  in sync). (b) Each wrapper replaces its alias with its own type carrying an
  unknown arm that a new variant maps to until the wrapper learns it. (c) Record
  a new variant as breaking for the foreign bindings. An agent cannot choose:
  it sets the public shape in six languages.
