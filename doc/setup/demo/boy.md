---
title: MoQ Boy
description: Crowd-controlled Game Boy Color streaming via MoQ.
---

# MoQ Boy

A "Twitch Plays" style demo where Game Boy Color games run server-side and stream live video + audio to web viewers. Anyone can send inputs — all inputs are applied immediately (anarchy mode). Multiple emulator instances can run simultaneously with different ROMs.

This demo showcases MoQ's key differentiators:

- **Prefix-based discovery** — game sessions are discovered automatically via announcement prefixes
- **Bidirectional communication** — viewers send button inputs back to the emulator
- **Low-latency video + audio** — H.264 video and Opus audio at native Game Boy resolution and framerate
- **Multi-viewer interaction** — all viewers see which buttons are being pressed in real-time

## Running

```bash
just dev boy
```

This starts three components in parallel:

1. **Relay** — a localhost MoQ relay server
2. **Emulator publisher** — a Rust binary running a Game Boy Color emulator, encoding video + audio
3. **Web viewer** — a Vite dev server with a browser UI

The default ROM ([Big2Small](https://github.com/mdsteele/big2small), a GPLv3 puzzle game) is downloaded automatically on first run.

### Custom ROM

```bash
just dev boy start rom=path/to/game.gb
```

### Multiple Sessions

Run additional instances in separate terminals with different ROMs:

```bash
just dev boy start rom=path/to/other.gb
```

Each session appears in the grid automatically via MoQ's announcement system.

## Controls

Click a session card to expand it, then:

- **Arrow keys** — D-pad
- **Z** — A button
- **X** — B button
- **Enter** — Start
- **Shift** — Select
- **Escape** — collapse the card

On-screen buttons are also available. All buttons highlight when pressed (by you or anyone else).

### Reset

- **Reset button** — any viewer can reset the game
- **Auto-reset** — the game resets after 5 minutes of inactivity, with a countdown warning

## Architecture

### Broadcast Hierarchy

```text
boy/
  {name}/                           <- game session broadcast
    catalog.json                    <- video + audio renditions (managed by moq-mux)
    video0.avc3                     <- 160x144 H.264 video at ~60fps
    audio0.opus                     <- Opus audio
    status                          <- JSON state (raw moq-lite track)
    viewer/
      {viewerId}/                   <- viewer broadcast
        command                     <- JSON commands (raw moq-lite track)
```

### Discovery

- **Viewers discover sessions** — subscribe to announcements with prefix `boy/`, filter to single-component suffixes
- **Emulator discovers viewers** — subscribes to `boy/{name}/viewer/` prefix using `OriginProducer::with_root()`

### Auto-Pause

Emulation and encoding are automatically paused when no viewers are watching. When a viewer connects, emulation resumes immediately with a fresh keyframe.

### Video Pipeline

The Rust publisher runs a Game Boy Color emulator (boytacean), grabs the framebuffer each frame, and encodes a single rendition:

| Track | Codec | Resolution | Framerate | Method |
|-------|-------|-----------|-----------|--------|
| Video | H.264 (avc3) | 160x144 | ~60fps | RGBA -> YUV -> encode via ffmpeg-next |

The web viewer upscales with CSS `image-rendering: pixelated` for crisp pixel art.

### Audio Pipeline

The emulator's APU outputs PCM audio samples which are encoded to Opus via ffmpeg-next and published through `moq_mux::import::Opus`.

### Metadata Tracks

| Track | Format | Content |
|-------|--------|---------|
| `status` | Raw JSON | `{"buttons": ["up", "a"], "reset_in": 295}` |
| `command` | Raw JSON | `{"type": "button", "button": "left"}` or `{"type": "reset"}` |

These tracks bypass the hang container format — they're raw UTF-8 JSON bytes written directly to `moq_lite` groups.

## Source Code

- **Rust publisher**: [`dev/boy/src/`](https://github.com/moq-dev/moq/tree/main/dev/boy/src/) — `main.rs`, `emulator.rs`, `video.rs`, `audio.rs`, `input.rs`
- **Web viewer**: [`dev/boy/src/index.ts`](https://github.com/moq-dev/moq/tree/main/dev/boy/src/index.ts)
- **Justfile**: [`dev/boy/justfile`](https://github.com/moq-dev/moq/tree/main/dev/boy/justfile)
