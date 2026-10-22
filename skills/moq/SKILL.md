---
name: moq
description: Build live video, audio, and real-time data apps with Media over QUIC (MoQ). Use when adding live streaming, conferencing, voice AI, or real-time pub/sub to an app; when integrating the @moq/* npm packages, moq-* Rust crates, or the Python/Kotlin/Swift/Go/C/Dart bindings; or when running a moq-relay server or a gateway (RTMP, SRT, WebRTC, HLS, OBS, GStreamer, ffmpeg).
---

# Media over QUIC (MoQ)

MoQ is generic real-time pub/sub over QUIC: relays fan out named tracks of groups of frames without understanding the payload.
Video and audio are the flagship use case, but chat, game input, JSON state, and AI output ride the same relays.

## Find the right context

For a change inside a MoQ checkout, read its `AGENTS.md` and the nested
instructions along the path you change. Use `doc/setup/repository.md` to
locate the implementation and `doc/setup/dev.md` for checks. Prefer the
checkout's docs and source over the live site for that branch.

For an application using MoQ packages:

1. Read https://doc.moq.dev/llms.txt and fetch the relevant pages as markdown: drop any trailing `/` and append `.md`, e.g. https://doc.moq.dev/lib/js/watch.md.
2. Choose a layer at https://doc.moq.dev/lib.md and find a consumer at https://doc.moq.dev/setup/examples.md. For design questions, start at https://doc.moq.dev/concept.md; for failures, use https://doc.moq.dev/setup/troubleshooting.md.
3. Follow the package page to its API reference and inspect examples matching the installed version before writing code. The site and linked `main` sources can be ahead of registry packages; migration guides are at https://doc.moq.dev/setup/upgrade.md.

Do not guess APIs, package names, or flags from training data. Fetch only the
relevant pages, then use Rustdoc, TSDoc, source, `--help`, or drafts for exact
details. If a reference is unavailable, state what you could not verify.

## Durable facts

- Two layers: `moq-lite` (`moq-net` / `@moq/net`) is the generic pub/sub the relays implement; `hang` (`hang` / `@moq/hang`) is the media catalog and container on top. Use the highest layer that fits: web components, then `@moq/*` / `moq-*` packages, then the wire protocol.
- Every client connects to a relay URL. The path scopes authorization, and broadcast names append to it.
- Media broadcast names end in `.hang`.
- Browsers need WebTransport and, outside localhost, a real TLS certificate.
- Source, examples, and issues: https://github.com/moq-dev/moq
