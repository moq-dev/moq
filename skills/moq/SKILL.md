---
name: moq
description: Build live video, audio, and real-time data apps with Media over QUIC (MoQ). Use when adding live streaming, conferencing, voice AI, or real-time pub/sub to an app; when integrating the @moq/* npm packages, moq-* Rust crates, or the Python/Kotlin/Swift/Go/C/Dart bindings; or when running a moq-relay server or a gateway (RTMP, SRT, WebRTC, HLS, OBS, GStreamer, ffmpeg).
---

# Media over QUIC (MoQ)

MoQ is generic real-time pub/sub over QUIC: relays fan out named tracks of groups of frames without understanding the payload.
Video and audio are the flagship use case, but chat, game input, JSON state, and AI output ride the same relays.

## Read the live docs

The project moves quickly and your training data is stale: it describes the IETF `moq-transport` draft and crates that no longer exist.
Do not guess APIs, package names, or flags. Read https://doc.moq.dev as part of the task.

1. Fetch https://doc.moq.dev/llms.txt, an index of every page with a one-line description.
2. Fetch the pages you need as markdown: drop any trailing `/` and append `.md`, e.g. https://doc.moq.dev/lib/js/watch.md.
3. For an API, read that package's page before writing code. For a design question, start at https://doc.moq.dev/concept.md.

## Durable facts

- Two layers: `moq-lite` (`moq-net` / `@moq/net`) is the generic pub/sub the relays implement; `hang` (`hang` / `@moq/hang`) is the media catalog and container on top. Use the highest layer that fits: web components, then `@moq/*` / `moq-*` packages, then the wire protocol.
- Every client connects to a relay URL. The path scopes authorization, and broadcast names append to it.
- Media broadcast names end in `.hang`.
- Browsers need WebTransport and, outside localhost, a real TLS certificate.
- Source, examples, and issues: https://github.com/moq-dev/moq
