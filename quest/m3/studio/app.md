# [XL] The studio app

## Goal

`moq-studio` runs on Linux, macOS, and Windows with docked panels:
- scenes and sources
- a preview and program pair with cut and fade
- device pickers
- an audio bus panel with gain and mute

Sources are a camera, display or window capture, a microphone, system audio
where moq-audio captures it (macOS today; the Linux and Windows capture quests
add the rest), an image, a solid color, and a subscribed MoQ broadcast. One canvas
publishes to a MoQ relay. A scene collection saves to a diffable file and
reloads.

`rs/moq-scene` (scene graph, sources, compositing) and moq-audio's program
bus land here, with the app as their consumer. Their API surface is only what
the app uses.

## Plan

- **Rendering:**
  - One wgpu device is shared by moq-video's render import paths (dmabuf,
    Metal) and egui-wgpu.
  - Preview and program are textures registered with egui, with no readback.
  - Program frames reach moq-video's encoder as a GPU `Surface` wherever the
    backend accepts one. Any CPU readback fallback shows in stats.
- **Timing:** output timestamps come from the program clock, not from source
  timestamps. A stalled source repeats its last frame rather than stalling
  the canvas.
- **Scene file:** choose the format in this quest. It must be diffable and
  carry a schema version, and loading an unknown version fails loudly. It
  never holds credentials.
- **Tests:** compositing tests render on a wgpu fallback adapter (llvmpipe or
  WARP), so they run in CI without a GPU.
- **Same-PR updates:**
  - `doc/bin/studio.md`
  - an AGENTS.md cross-package row for `rs/moq-studio` and `rs/moq-scene`
  - the repository map in `doc/setup/repository.md`
- **Splitting:** if this proves too large for one PR, split along moq-scene
  versus app, as long as each piece still lands with a consumer.

## Related

- [Hardware validation](/quest/m3/video-hardware.md) - capture and zero-copy paths not yet run on real machines
