---
title: Demos
description: The web demo, MoQ Boy, CARLA, and VR
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
- **Discovery by prefix.** Game sessions appear under `boy/game/`; viewers each publish a tiny broadcast under `boy/viewer/`. No control plane.
- **Bidirectional data.** Button presses go up on a JSON `command` track; a `status` track comes down with everyone's held buttons and latency. Same relay, same protocol as the media.
- **Split trust.** In production the game prefix is authenticated (only the server publishes games) while the viewer prefix is anonymous.

Code: [`rs/moq-boy`](https://github.com/moq-dev/moq/tree/main/rs/moq-boy) (emulator
and publisher) and [`demo/boy`](https://github.com/moq-dev/moq/tree/main/demo/boy)
(the `<moq-boy>` element and page).

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

## VR

A WebXR player for 360 and VR180 video with spatial audio, in a headset or a
desktop tab. It lays a broadcast out by the catalog's
[spatial fields](/concept/hang#catalog): an equirectangular `projection` wraps
the picture on a sphere spanning its field of view, `stereo` crops each eye
from a packed frame, and an `ambisonic` audio layout decodes the sound field
around the viewer's head, at first order: a higher order plays only its first
four channels. Without them it shows a flat screen with the sound
coming from it.

```bash
just relay    # a local relay
ffmpeg -re -i vr180.mp4 -c copy -strict unofficial \
    -movflags cmaf+frag_keyframe+empty_moov+default_base_moof -f mp4 - |
    moq --connect http://localhost:4443 --broadcast vr.hang import fmp4
just vr serve # the player: Enter VR in a headset, or drag to look around
```

The importer reads the projection and stereo packing from an MP4's Spherical
Video V2 boxes, which ffmpeg writes only past `-strict unofficial`, and the
ambisonic layout from Opus channel mapping family 2. In the player, a trigger
or `R` recentres the picture where you look, and a grip or `M` mutes.

Code: [`demo/vr`](https://github.com/moq-dev/moq/tree/main/demo/vr).
