---
title: moq-boy
description: Crowd-controlled Game Boy Color emulator that streams over MoQ
---

# moq-boy

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

A Rust binary that runs a Game Boy Color emulator, encodes its framebuffer and audio as MoQ tracks, and accepts button input from any number of viewers.

This is the server side of the [MoQ Boy demo](/demo/moq-boy). See that page for the architecture breakdown — broadcast layout, tracks, auto-pause, etc. The browser-side counterpart is [@moq/boy](/js/@moq/boy).

## Highlights

- **On-demand emulation** — the emulator only runs while at least one viewer is subscribed to the video track.
- **Hang container format** — video and audio flow through [moq-mux](/rs/crate/moq-mux) so standard `hang` players can consume them.
- **Raw JSON metadata** — `status` and `command` tracks bypass the container and publish JSON directly to [moq-lite](/rs/crate/moq-lite) groups.

## Running Locally

```bash
just demo boy
```

This starts a localhost relay, the emulator, and a Vite dev server. A default ROM is downloaded on first run. To load a custom ROM:

```bash
just demo boy start path/to/game.gb
```

See the [setup guide](/setup/demo/boy) for controls, reset behavior, and the authenticated/anonymous prefix split.

## Source

[rs/moq-boy](https://github.com/moq-dev/moq/tree/main/rs/moq-boy)

## Next Steps

- Read the [demo architecture](/demo/moq-boy) for tracks, broadcast layout, and how discovery works
- See the [@moq/boy](/js/@moq/boy) package for the browser player
- See [moq-lite](/rs/crate/moq-lite) for the transport layer and [moq-mux](/rs/crate/moq-mux) for the container format
