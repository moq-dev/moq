---
title: MoQ Boy
description: Architecture and track layout of the MoQ Boy demo
---

# MoQ Boy

A crowd-controlled Game Boy Color emulator that streams over MoQ. The server emulates and encodes on-demand, the browser subscribes to whichever games are visible, and any viewer can send inputs back. Live at [moq.dev/boy](https://moq.dev/boy); local setup instructions are in the [setup guide](/setup/demo/boy).

This page documents what the demo demonstrates and how its tracks are wired together. For the running-locally instructions see [setup/demo/boy](/setup/demo/boy); for the code see the [moq-boy](/rs/crate/moq-boy) crate and the [@moq/boy](/js/@moq/boy) package.

## What it demonstrates

- **On-demand encoding** — emulation and encoding only run while at least one viewer is subscribed. Scroll the video off-screen and the server pauses within a frame or two. A fresh keyframe is sent on resume.
- **Prefix-based discovery** — no control plane. Games and players are discovered by subscribing to announcement prefixes.
- **Bidirectional communication** — viewers publish a tiny track of their own so the emulator can receive button presses. Same protocol, same relay, no extra plumbing.
- **Shared input** — every viewer sees which buttons are being held down, by whom, with live latency numbers.
- **Low-latency A/V** — native Game Boy resolution (160x144) at ~60fps with sub-frame latency on a LAN.

## Broadcast layout

Two prefixes: one for the emulator, one for viewers.

```text
{gamePrefix}/
  {name}/                       <- one session per running emulator
    catalog.json                <- video + audio renditions (managed by moq-mux)
    video0.avc3                 <- 160x144 H.264 at ~60fps
    audio0.opus                 <- Opus audio
    status                      <- JSON state (raw moq-lite track)

{viewerPrefix}/
  {name}/
    {viewerId}/                 <- one broadcast per connected viewer
      command                   <- JSON commands (raw moq-lite track)
```

| Environment | Game prefix | Viewer prefix |
|-------------|-------------|---------------|
| **Localhost** | `anon/boy/game` | `anon/boy/viewer` |
| **Production** | `demo/boy/game` (authenticated) | `anon/boy/viewer` (unauthenticated) |

Splitting authenticated and unauthenticated traffic lets the server be the only thing that can publish a game, while anyone can show up and play.

## Media tracks

The video and audio tracks go through [moq-mux](/rs/crate/moq-mux), which handles the [hang](/rs/crate/hang) container format (catalog, codec init, group boundaries).

| Track | Codec | Resolution | Framerate | Pipeline |
|-------|-------|-----------|-----------|----------|
| `video0.avc3` | H.264 (avc3) | 160x144 | ~60fps | RGBA → YUV → encode via `ffmpeg-next` |
| `audio0.opus` | Opus | — | — | APU PCM → encode via `ffmpeg-next` → `moq_mux::import::Opus` |

The browser upscales video with CSS `image-rendering: pixelated` so the pixel art stays crisp.

## Metadata tracks

The `status` and `command` tracks bypass the container format — they're raw UTF-8 JSON bytes written directly to [moq-lite](/rs/crate/moq-lite) groups.

| Track | Direction | Format |
|-------|-----------|--------|
| `status` | Server → viewers | `{"buttons": ["up", "a"], "latency": {"abc123": 42}}` |
| `command` | Viewer → server | `{"type": "buttons", "buttons": ["left"]}` or `{"type": "reset"}` |

Every viewer subscribes to the server's `status` track so they see shared input. The server subscribes to the viewer prefix via `OriginProducer::with_root()` and fans in all `command` tracks, so adding a new viewer is just a new announcement.

## Auto-pause

When the last viewer unsubscribes from a session's video track, the emulator stops advancing the CPU and the encoder stops running. On the next subscribe, emulation resumes and a keyframe is pushed immediately.

This works because subscriptions are a first-class signal in MoQ — the emulator doesn't need heuristics or timers, it just watches whether anyone is consuming the track.

## Source code

- **Rust emulator/publisher**: [rs/moq-boy](https://github.com/moq-dev/moq/tree/main/rs/moq-boy) — emulator loop, video/audio encoding, input handling
- **Browser player**: [js/moq-boy](https://github.com/moq-dev/moq/tree/main/js/moq-boy) — discovery, rendering, input capture, UI
- **Demo wiring**: [demo/boy](https://github.com/moq-dev/moq/tree/main/demo/boy) — the HTML page that mounts `<moq-boy>`

## Related

- [moq-boy crate](/rs/crate/moq-boy) — the Rust binary
- [@moq/boy package](/js/@moq/boy) — the browser player
- [setup/demo/boy](/setup/demo/boy) — how to run it locally, controls, custom ROMs
- [moq-lite](/rs/crate/moq-lite) — the transport layer used by every track here
- [moq-mux](/rs/crate/moq-mux) — the container format used for A/V
