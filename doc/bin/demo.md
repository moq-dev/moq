---
title: Demos
description: The web demo and MoQ Boy
---

# Demos

Both run locally from a checkout and live at [moq.dev](https://moq.dev).

## Web demo

```bash
just            # relay + Big Buck Bunny + the web UI at http://localhost:5173
```

A Vite app in [`demo/web/`](https://github.com/moq-dev/moq/tree/main/demo/web)
that uses the [`<moq-watch>`](/lib/js/watch) and
[`<moq-publish>`](/lib/js/publish) web components. It shows:

- **Watching** a live broadcast with an adjustable latency budget and a stats overlay.
- **Publishing** your camera, microphone, screen, or a file from the browser with WebCodecs.
- **Discovery**: broadcasts appear as they're announced under the prefix.

`just web serve https://cdn.moq.dev/anon` points it at the public relay
instead of a local one.

## MoQ Boy

A crowd-controlled Game Boy Color. The emulator runs server-side, streams video
and audio over MoQ, and any viewer can press buttons. Live at
[moq.dev/boy](https://moq.dev/boy).

```bash
just boy                              # relay + emulator + viewer
just boy start path/to/game.gb        # your own ROM; run several for a grid
```

What it demonstrates:

- **On-demand encoding.** Emulation and encoding pause when the last viewer leaves and resume with a keyframe when one arrives. No timers, just subscription state.
- **Discovery by prefix.** Game sessions appear under `boy/game/`; viewers each publish a tiny broadcast under `boy/viewer/`. No control plane.
- **Bidirectional data.** Button presses go up on a raw JSON `command` track; a `status` track comes down with everyone's held buttons and latency. Same relay, same protocol as the media.
- **Split trust.** In production the game prefix is authenticated (only the server publishes games) while the viewer prefix is anonymous.

Tracks: `catalog.json`, `video0.avc3` (160x144 H.264 at 60 fps), `audio0.opus`,
and the raw `status` and `command` tracks. Code:
[`rs/moq-boy`](https://github.com/moq-dev/moq/tree/main/rs/moq-boy) (emulator
and publisher), [`js/moq-boy`](https://github.com/moq-dev/moq/tree/main/js/moq-boy)
(the `<moq-boy>` element, published as `@moq/boy`), and
[`demo/boy`](https://github.com/moq-dev/moq/tree/main/demo/boy). The same
shape covers teleoperation: swap the emulator for a robot.
