---
title: Demos
description: The web demo, MoQ Boy, and CARLA
---

# Demos

Each runs locally from a checkout. The web demo and MoQ Boy also live at
[moq.dev](https://moq.dev); CARLA's simulator needs a Linux host with an NVIDIA GPU.

## Web demo

```bash
just            # relay + Big Buck Bunny + the web UI at http://localhost:5173/watch.html
```

A Vite app in [`demo/web/`](https://github.com/moq-dev/moq/tree/main/demo/web)
that uses the [`<moq-watch>`](/lib/js/watch) and
[`<moq-publish>`](/lib/js/publish) web components. It shows:

- **Watching** a live broadcast with an adjustable latency budget and a stats overlay.
- **Publishing** your camera, microphone, screen, or a file from the browser with WebCodecs.
- **Meeting**: a multi-participant room (`meet.html`) using `@moq/room`. Open the same room in two tabs.
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
- **Discovery by prefix.** Game sessions appear under `boy/game/`. No control plane.
- **Sockets.** Each emulator runs a [socket](/concept/socket) server endpoint named for its game, and each viewer dials it from a client endpoint with a random id. Button presses go up as messages on the viewer's own session, and the emulator answers each with that viewer's latency. A `status` track comes down to everyone with the held buttons and player count. Same relay, same protocol as the media. Sockets need `moq-lite-07-wip` or moq-transport on every hop. The emulator and viewer offer `moq-lite-07-wip`, which is opt-in, so the local relay enables it.
- **Split trust.** In production only the server publishes games and their server endpoints, while anonymous viewers may publish only client endpoints.

Code: [`rs/moq-boy`](https://github.com/moq-dev/moq/tree/main/rs/moq-boy) (emulator
and publisher), [`js/moq-boy`](https://github.com/moq-dev/moq/tree/main/js/moq-boy)
(the `<moq-boy>` element, published as `@moq/boy`), and
[`demo/boy`](https://github.com/moq-dev/moq/tree/main/demo/boy). The same
shape covers teleoperation: swap the emulator for a robot.

## CARLA

Teleoperation over MoQ: simulated trucks drive themselves through one
[CARLA](https://carla.org) world until you take one over from the browser.
The simulator needs a Linux host with an NVIDIA GPU and an image built by
[moq-dev/carla](https://github.com/moq-dev/carla); the player runs anywhere.

```bash
just carla venv                       # once: the worker
CARLA_IMAGE=<tag> just carla server   # leave the simulator running
just carla                            # relay + trucks + player
```

What it demonstrates:

- **GPU to wire.** Camera images never leave the GPU: `moq-video` encodes them on the device that rendered them, and a camera renders only while someone subscribes.
- **Control with a lease.** A driver dials the truck's [socket](/concept/socket) endpoint and sends JSON controls on its own session, which carries the truck's acknowledgements back; the truck accepts one driver at a time, bound to that session, and returns to autonomy when the lease lapses.
- **Measured feedback.** Each truck's `status` track carries what the gate accepted and what the vehicle did, so the player draws requested against measured steering.

Code: [`demo/carla`](https://github.com/moq-dev/moq/tree/main/demo/carla).
