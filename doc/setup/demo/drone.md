---
title: Drone Demo
description: Simulated drone fleet with live video and remote control via MoQ.
---

# Drone Demo

A simulated drone fleet where each drone renders a 2D game scene with physics, publishes live video and sensor telemetry, while web viewers can discover drones, watch their feeds, and send control commands.

This demo showcases MoQ's key differentiators:
- **Prefix-based discovery** — drones are discovered automatically via announcement prefixes
- **Bidirectional metadata** — sensor telemetry and control commands flow alongside video
- **Low-latency video** — H.264 video with simulcast (720p + 360p)
- **Multi-viewer awareness** — multiple controllers are tracked and displayed

## Running

```bash
just dev::drone
```

This starts three components in parallel:
1. **Relay** — a localhost MoQ relay server
2. **Drone publisher** — a Rust binary that renders a 2D physics game, encodes video, and publishes sensor data
3. **Web viewer** — a Vite dev server with a browser UI

Once running, open the browser to see the drone grid.

### Multiple Drones

```bash
just dev::drone 3
```

Spawns 3 drone instances, each with a unique ID. They appear in the grid automatically via MoQ's announcement system.

## Controls

Click a drone card to expand it, then:
- **Arrow keys** — move the drone on the 5x5 grid
- **Spacebar** — grab/drop a ball
- **Dock button** — auto-navigate to the docking station
- **Escape** — collapse the card

The drone has a battery that drains while flying and recharges on the dock. At 10% battery, it auto-docks.

## Architecture

### Broadcast Hierarchy

```text
drone/
  {id}/                           ← drone broadcast
    catalog.json                  ← video renditions (managed by moq-mux)
    video0.avc3                   ← 720p video (encoded from rendered frames)
    video1.avc3                   ← 360p video (downscaled + re-encoded)
    sensor                        ← JSON telemetry (raw moq-lite track)
    status                        ← JSON drone state (raw moq-lite track)
    viewer/
      {viewerId}/                 ← viewer broadcast
        command                   ← JSON commands (raw moq-lite track)
```

### Discovery

- **Viewers discover drones** — subscribe to announcements with prefix `drone/`, filter to single-component suffixes
- **Drones discover viewers** — subscribe to `drone/{id}/viewer/` prefix using `OriginProducer::with_root()` to auto-strip the prefix

### Video Pipeline

The Rust publisher renders a 2D game scene with tiny-skia and Rapier2D physics, then encodes two renditions:

| Track | Codec | Resolution | Bitrate | Method |
|-------|-------|-----------|---------|--------|
| HD | H.264 (avc3) | 720x720 | 500 kbps | RGBA → YUV → encode via ffmpeg-next |
| Preview | H.264 (avc3) | 360x360 | 200 kbps | Downscale + re-encode via ffmpeg-next |

Each rendition has its resolution burned into the bottom-right corner.
The web viewer selects renditions automatically based on a pixel budget — 360p for thumbnail cards, 720p when expanded.

### Metadata Tracks

| Track | Format | Content |
|-------|--------|---------|
| `sensor` | Raw JSON | `{"battery": 87, "temp": 42.1, "gps": [37.7, -122.4], "uptime": 3600}` |
| `status` | Raw JSON | `{"actions": [...], "controllers": ["v3x8k2"]}` |
| `command` | Raw JSON | `{"type": "action", "name": "left"}` or `{"type": "kill"}` |

These tracks bypass the hang container format — they're raw UTF-8 JSON bytes written directly to `moq_lite` groups.

## Source Code

- **Rust publisher**: [`dev/drone/src/`](https://github.com/moq-dev/moq/tree/main/dev/drone/src/) — `main.rs`, `drone.rs`, `game.rs`, `video.rs`, `sensor.rs`
- **Web viewer**: [`dev/drone/src/index.ts`](https://github.com/moq-dev/moq/tree/main/dev/drone/src/index.ts)
- **Justfile**: [`dev/drone/justfile`](https://github.com/moq-dev/moq/tree/main/dev/drone/justfile)
