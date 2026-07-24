---
name: moq
description: Build live video, audio, and real-time data apps with Media over QUIC (MoQ). Use when adding live streaming, conferencing, voice AI, or real-time pub/sub to an app; when integrating the @moq/* npm packages, moq-* Rust crates, or the Python/Kotlin/Swift/Go/C bindings; or when running a moq-relay server or a gateway (RTMP, SRT, WebRTC, HLS, OBS, GStreamer, ffmpeg).
---

# Media over QUIC (MoQ)

MoQ is a live media protocol delivering real-time (sub-second) latency at CDN scale.
It is generic pub/sub over QUIC: video and audio are the flagship use case, but any live data rides the same relays (chat, game input, JSON state, AI output).

This skill is an index of the moq-dev monorepo (https://github.com/moq-dev/moq): what you can build and which pieces to reach for. Fetch the linked docs for details instead of guessing APIs.

## The two layers

- **moq-lite**: generic real-time pub/sub. A *broadcast* contains named *tracks*; tracks carry *groups* of *frames*. Relays cache, deduplicate, and fan out without understanding the payload, so anything can be end-to-end encrypted. Rust `moq-net`, TypeScript `@moq/net`.
- **hang**: media on top. A catalog track describes codecs/renditions; containers carry timestamped codec bitstream. Rust `hang`, TypeScript `@moq/hang`. Think of hang as HLS/DASH and moq-lite as HTTP.

Everything below builds on these two. Custom applications can extend hang with their own tracks or replace it entirely.

## What you can build

**Live streaming (Twitch-style)**. Ingest via the OBS plugin (`cpp/obs`), RTMP (`rs/moq-rtmp`), SRT (`rs/moq-srt`), GStreamer (`rs/moq-gst`), or pipe ffmpeg into the `moq` CLI (`rs/moq-cli`, a media router bridging fMP4/CMAF, MPEG-TS, FLV, HLS, RTMP, SRT, WebRTC). Distribute through `moq-relay` and watch in the browser with `<moq-watch>` (`@moq/watch`). Serve legacy players via the HLS/LL-HLS gateway (`rs/moq-hls`) and build a rendition ladder with just-in-time transcoding (`rs/moq-transcode`). Docs: https://doc.moq.dev/bin/

**Conferencing (Zoom-style)**. Browser capture and playback with `@moq/publish` + `@moq/watch`; each participant is a broadcast, discovered via announcements. Bridge existing WebRTC clients with the WHIP/WHEP gateway (`rs/moq-rtc`). Why MoQ over WebRTC: https://doc.moq.dev/concept/use-case/conferencing

**Voice/video AI agents**. Server-side media without a browser: `rs/moq-video` and `rs/moq-audio` do native capture, hardware encode/decode (VideoToolbox, D3D11, NVENC, VAAPI). Python bindings fit ML stacks. TTS output written faster than real-time plays smoothly via `@moq/watch` buffered playback (`latency-max`). Background: https://moq.dev/blog/webrtc-is-the-problem and https://doc.moq.dev/concept/use-case/ai

**Real-time data, no media at all**. Chat, multiplayer game state, telemetry, collaborative apps: publish raw tracks with `@moq/net` or `moq-net`. Helpers: `moq-json` / `@moq/json` (JSON snapshots, merge-patch deltas, append logs) and `moq-flate` / `@moq/flate` (compression). A synchronized-clock example lives in `js/clock`. Same relays, same auth as media.

**Interactive streams (Twitch-plays)**. Combine both directions: media down, input tracks up. MoQ Boy (`rs/moq-boy`, `demo/boy`) is a crowd-controlled Game Boy doing exactly this: https://moq.dev/blog/moq-boy

**Cloud gaming / remote desktop**. Screen capture and low-latency hardware encoding via `rs/moq-video` (camera and display sources on macOS/Windows/Linux, plus window capture), sub-100ms playback in the browser.

**Native and mobile apps**. One Rust core (`rs/moq-ffi`) with idiomatic bindings: Python (asyncio), Kotlin (coroutines/Flow, Android), Swift (async sequences, iOS/macOS), Go, and C (`rs/libmoq` for C/C++ build systems). Index: https://doc.moq.dev/lib/

**Your own CDN**. `moq-relay` clusters across regions (https://doc.moq.dev/bin/relay/cluster), authenticates with path-scoped JWTs (`moq-token`, https://doc.moq.dev/bin/relay/auth), and exposes live traffic stats as MoQ tracks (`rs/moq-stats`). Load-test with `rs/moq-bench`. Or skip self-hosting: moq-lite is forwards-compatible with IETF moq-transport, so it runs over third-party CDNs like Cloudflare (https://moq.dev/blog/first-cdn). Native apps can also connect P2P via Iroh.

**Web playback/capture on any site**. `<moq-watch>` and `<moq-publish>` are plain web components with optional UI overlays; embeddable with no build step. Docs: https://doc.moq.dev/lib/js/@moq/watch and https://doc.moq.dev/lib/js/@moq/publish

## Repo map

- `rs/` Rust: `moq-net` (pub/sub), `hang` (media), `moq-relay`, `moq-cli` (installs a `moq` binary), `moq-native` (QUIC/TLS setup), `moq-mux` (container muxing), `moq-token[-cli]` (auth), gateways (`moq-rtmp`, `moq-srt`, `moq-rtc`, `moq-hls`), native codecs (`moq-video`, `moq-audio`, `moq-nvenc`), `moq-ffi`/`libmoq` (bindings core).
- `js/` TypeScript, published as `@moq/*`: `net`, `hang`, `watch`, `publish`, `token`, `json`, `signals`.
- `py/`, `swift/`, `kt/`, `go/` bindings; `cpp/obs` OBS plugin; `demo/` runnable demos.

Note: some crates are in-tree but not yet on crates.io (e.g. `moq-video`, `moq-transcode`); check https://crates.io before depending on one, or build from the repo.

## Getting started

- Every client connects to a relay URL; the path scopes auth and broadcast names append to it. Public dev relay: `https://cdn.moq.dev/anon` (unauthenticated, testing only, pick a unique name). Local: `cargo install moq-relay`, or clone the repo and `nix develop -c just` to run a relay, demo media, and web UI together.
- Media broadcast names end in `.hang` (selects the hang catalog; `.msf` selects the IETF MSF format).
- Browsers need WebTransport (Chrome/Edge; Firefox/Safari experimental) and, outside localhost, a real TLS certificate.
- Auth is a JWT in the `?jwt=` query parameter, signed by `moq-token`.

## Reference

- Documentation: https://doc.moq.dev (concepts, apps, libraries, relay ops)
- Rust API docs: https://docs.rs/moq-net and https://docs.rs/hang
- Runnable examples: https://github.com/moq-dev/moq/tree/main/js/net/examples and https://github.com/moq-dev/moq/tree/main/rs/hang/examples
- Protocol drafts: https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html
- Inspiration: https://moq.dev/blog/
