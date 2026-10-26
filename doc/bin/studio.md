---
title: Studio
description: Compose scenes on the GPU and go live over MoQ from a native app
---

# Studio

`moq-studio` is a native streaming studio for Linux, macOS, and Windows. It
composes cameras, screens, windows, images, colors, and broadcasts from a relay
into scenes on the GPU, mixes their audio, and publishes the result to a MoQ
relay. It is early and not packaged yet, so build it from source:

```bash
cargo run --release -p moq-studio --features capture,openh264 -- \
    show.toml --connect https://cdn.moq.dev/anon --broadcast my-show
```

- `show.toml` is the scene collection. It is created on the first save.
- Without `--connect`, the studio composes and previews but publishes nothing,
  and broadcast sources stay offline.
- `capture` adds camera, display, window, microphone, and system-audio sources.
  On Linux it links ALSA; add `pipewire` for PipeWire cameras. `openh264`
  is the software H.264 fallback for a machine without a hardware encoder.
- On Linux, display and window sources capture X11 and XWayland only; a
  Wayland screen or window cannot be picked yet.
- The `--connect-*`, `--quic-*`, and `--log-level` flags are moq-cli's; see
  `moq-studio --help`.

## Panels

Every panel is a tab you can drag, split, or stack.

| Panel | Use it to |
| --- | --- |
| Preview, Program | See the scene you are editing and the one that is live. **Cut** puts the preview on program at once; **Fade** blends over the collection's fade time. |
| Scenes | Pick the scene in preview, add one, or remove one. |
| Items | Place, show, hide, and order the sources the preview scene draws, back to front. |
| Sources | Add a source by hand, see whether each is live, and remove one. |
| Devices | List this machine's cameras, displays, windows, and audio inputs, and add one as a source. |
| Audio | Set each audio source's gain and mute, and watch its meter and the program's. |
| Output | See the relay connection, whether anyone is watching, and the encoder's work. |

A broadcast source subscribes through the same relay connection, plays the
best video and audio its catalog lists, switches when the catalog changes, and
comes back on its own when the publisher restarts. Audio from every source is mixed into the program whether
or not its scene is live; mute a source to leave it out.

## Scene file

The collection is TOML, so it diffs and reviews like code. It names devices,
files, and broadcast paths, never a relay URL or token. Check its image paths
and device IDs before sharing it, since they can name your home directory or
hardware. A file from another format version fails to load rather than being
guessed at.

```toml
version = 1

[canvas]
width = 1920
height = 1080
framerate = 30

[transition]
fade = "300ms"

[sources.camera]
kind = "camera"

[sources.logo]
kind = "image"
path = "logo.png" # relative to the scene file

[sources.guest]
kind = "broadcast"
path = "room/guest"

[audio.guest]
gain = 0.5

[[scenes]]
name = "Main"
items = [
    { source = "camera", rect = { x = 0, y = 0, width = 1920, height = 1080 } },
    { source = "logo", rect = { x = 1600, y = 40, width = 280, height = 120 } },
]
```

Source kinds are `camera`, `display`, and `microphone` (each with an optional
`id`, the default device without one), `window` (with an `id`), `system_audio`
(macOS), `image` (PNG or JPEG), `color` (`#rrggbb` or `#rrggbbaa`), and
`broadcast`. The Devices panel fills in ids. **File > Save** (Ctrl+S) writes
the file and **File > Reload** reads it back; a new canvas size or frame rate
takes a restart.

## Output

The program publishes as one H.264 rendition at the canvas size and frame rate,
with Opus stereo audio, under the `--broadcast` path. Timestamps come from the
studio's own clock, so a source that stalls repeats its last picture instead of
holding up the stream.

Video is only encoded while someone watches it. Each frame is read back from
the GPU to the CPU for the encoder, which the Output panel counts; frames the
encoder cannot keep up with are dropped there rather than delaying the stream.

Sources make the opposite trip. A frame already on the GPU is drawn where it
lies: PipeWire camera and VA-API decode DMA-BUFs on Linux, given a Vulkan
driver that imports them, and NV12 capture and VideoToolbox decode surfaces on
macOS. Every other source, every source on Windows, and every still image
delivers its frames from the CPU, which the compositor uploads.
