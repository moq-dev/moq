<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)
[![Discord](https://img.shields.io/discord/1124083992740761730)](https://discord.moq.dev)
[![Crates.io](https://img.shields.io/crates/v/moq-net)](https://crates.io/crates/moq-net)
[![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)

# Media over QUIC

[Media over QUIC](https://moq.dev) (MoQ) is an open-source live media and real-time data stack built on QUIC and WebTransport. One protocol replaces WebRTC, HLS/DASH, RTMP/SRT, and custom WebSocket fan-out, with **sub-second latency** at **CDN scale**.

- **Sub-second, no buffering spinners**: every group of frames rides its own QUIC stream. Under congestion, old video gets skipped while audio and fresh frames sail through.
- **CDN scale, not SFU pain**: relays cache and fan out to millions of viewers without parsing media. No SDP, no ICE, no TURN; just dial a URL.
- **Pull, not push**: nothing leaves a publisher until someone subscribes. Have 1,000 cameras? Only encode and upload the ones anyone is actually watching.
- **Per-viewer latency budgets**: 100 ms for a live call or remote operator, 10 s for bad Wi-Fi, or a pristine archive recording, all from the same broadcast and relay.
- **Media plus any live data**: video, audio, chat, AI transcripts, game inputs, and telemetry share one connection with explicit priorities.
- **Every platform, migrate one piece at a time**: Rust core, from-scratch TypeScript for browsers (WebTransport + WebCodecs with automatic WebSocket fallback), 7 native language bindings, and drop-in gateways for WebRTC, HLS/DASH, RTMP, SRT, and RTSP.

> **Standards & CDNs:** This repo implements [moq-lite](https://doc.moq.dev/concept/moq-lite) and [hang](https://doc.moq.dev/concept/hang) with compatibility for the IETF [moq-transport](https://doc.moq.dev/concept/standard) drafts. Self-host [`moq-relay`](rs/moq-relay) anywhere, use a managed CDN like [moq.pro](https://moq.pro) or [Cloudflare](https://moq.dev/blog/first-cdn), or mix both on the same broadcast.

## Try It Live

All of the hosted demos at **[moq.dev/demo](https://moq.dev/demo)** run on the open-source code in this repository:

- **[Voice AI](https://moq.dev/voice)**: talk to a Pipecat voice agent over MoQ with faster-than-real-time TTS bursts, instant barge-in, and live transcripts (no WebRTC).
- **[CARLA Teleoperation](https://moq.dev/carla)**: steer simulated trucks in real time with on-demand cameras, vehicle controls, and telemetry on one connection.
- **[MoQ Boy](https://moq.dev/boy)**: crowd-controlled Game Boy (video down, button inputs up, sleeps when nobody is watching).
- **[hang.live](https://hang.live)**: multi-party video conferencing with spatial audio and chat, built without WebRTC.
- **[Watch](https://moq.dev/watch?project=demo\&name=bbb.hang)** & **[Publish](https://moq.dev/publish)**: browser playback (`<moq-watch>`) and webcam publishing (`<moq-publish>`).

Want a real-world story? See how [autonomous boats run MoQ over cellular and Starlink](https://moq.dev/robotics), or read the [blog](https://moq.dev/blog) ([Replacing WebRTC](https://moq.dev/blog/replacing-webrtc), [Replacing HLS/DASH](https://moq.dev/blog/replacing-hls-dash), [On a Boat](https://moq.dev/blog/on-a-boat)).

### Quick Start

Full documentation lives at **[doc.moq.dev](https://doc.moq.dev)**.

Install the `moq` CLI (macOS & Linux):

```sh
curl -fsSL https://moq.sh | sh
```

Also available via Homebrew, apt, dnf, winget, Docker, Cargo, and Nix; see the [install guide](https://doc.moq.dev/setup/install).

Or run the full local demo stack (relay, test stream, and web UI) from this checkout using [Nix](https://nixos.org/download.html) with [flakes](https://wiki.nixos.org/wiki/Flakes):

```sh
nix develop -c just
```

It opens the watch, publish, and stats pages in your browser (or visit <http://localhost:5173/watch.html>). No Nix? See the [quick start guide](https://doc.moq.dev/setup/).

## What You Can Build

Pick your use case to jump straight to the docs, tools, and source folders:

| Use case | Replaces | Tools & libraries in this repo | Docs |
| --- | --- | --- | --- |
| **Conferencing & rooms** | WebRTC, SFUs | [`@moq/room`](js/room), [`@moq/publish`](js/publish), [`@moq/watch`](js/watch), [`moq-webrtc`](rs/moq-webrtc), [`moq-relay`](rs/moq-relay) | [vs WebRTC](https://doc.moq.dev/concept/use-case/conferencing) |
| **Voice & video AI** | WebRTC, WebSockets | [Python `moq-rs`](py/moq-rs), [`@moq/watch`](js/watch), [`@moq/publish`](js/publish), [`@moq/json`](js/json) | [MoQ for AI](https://doc.moq.dev/concept/use-case/ai) |
| **Live streaming & distribution** | HLS, DASH | [`moq-relay`](rs/moq-relay), [`@moq/watch`](js/watch), [`moq-hls`](rs/moq-hls), [`moq-transcode`](rs/moq-transcode), [`moq-archive`](rs/moq-archive) | [vs HLS/DASH](https://doc.moq.dev/concept/use-case/distribution) |
| **Contribution & ingest** | RTMP, SRT, RTSP, WHIP | [OBS plugin](cpp/obs), [GStreamer](rs/moq-gst), [`moq-cli`](rs/moq-cli), [`moq-rtmp`](rs/moq-rtmp), [`moq-srt`](rs/moq-srt), [`moq-rtsp`](rs/moq-rtsp) | [vs RTMP/SRT](https://doc.moq.dev/concept/use-case/contribution) |
| **Robotics & teleoperation** | WebRTC, raw RTP | [`moq-net`](rs/moq-net), [`moq-video`](rs/moq-video), [`moq-audio`](rs/moq-audio), [`moq-json`](rs/moq-json), [C++](cpp/moq), [Python](py/moq-rs) | [Concepts](https://doc.moq.dev/concept/) |
| **Interactive & real-time data** | WebSockets, DataChannels | [`moq-net`](rs/moq-net), [`@moq/net`](js/net), [`moq-json`](rs/moq-json), [`@moq/json`](js/json) | [moq-lite](https://doc.moq.dev/concept/moq-lite) |

## What's in This Repository

### Applications, Plugins & Gateways

Ready-to-run binaries and plugins ([docs](https://doc.moq.dev/bin/)):

- **[`moq-relay`](rs/moq-relay)** ([docs](https://doc.moq.dev/bin/relay/)): scalable relay server with RAM caching, JWT auth, multi-region clustering, HTTP/metrics, and zero-downtime upgrades.
- **[`moq-cli`](rs/moq-cli)** (`moq`, [docs](https://doc.moq.dev/bin/cli)): all-in-one CLI media router to pipe FFmpeg, capture cameras and screens natively, play in a window, transcode ABR ladders on demand, record to S3/MP4, and [inspect relays](https://doc.moq.dev/bin/inspect).
- **Plugins & studio**: [OBS Studio plugin](cpp/obs) ([docs](https://doc.moq.dev/bin/obs)), [GStreamer plugin](rs/moq-gst) ([docs](https://doc.moq.dev/bin/gstreamer)), and [moq-studio](https://github.com/moq-dev/studio) (native GPU streaming studio).
- **Drop-in gateways**: bridge legacy stacks via `moq import` / `moq export` or embed the Rust crates directly: [WebRTC (WHIP/WHEP)](rs/moq-webrtc) ([docs](https://doc.moq.dev/bin/webrtc)), [HLS/DASH](rs/moq-hls) ([docs](https://doc.moq.dev/bin/hls)), [RTMP/E-RTMP](rs/moq-rtmp) ([docs](https://doc.moq.dev/bin/rtmp)), [SRT](rs/moq-srt) ([docs](https://doc.moq.dev/bin/srt)), [RTSP](rs/moq-rtsp), and [MPEG-TS](rs/moq-mux) ([docs](https://doc.moq.dev/bin/ts)).

### Libraries & Bindings

Two primary implementations (Rust and TypeScript) plus 7 native bindings, all speaking the same wire protocol ([library guide](https://doc.moq.dev/lib/)):

| Layer | Rust ([`rs/`](rs), [docs](https://doc.moq.dev/lib/rs/)) | TypeScript ([`js/`](js), [docs](https://doc.moq.dev/lib/js/)) |
| --- | --- | --- |
| Live pub/sub | [`moq-net`](rs/moq-net), [`moq-tokio`](rs/moq-tokio) | [`@moq/net`](js/net) |
| Media catalog & containers | [`hang`](rs/hang), [`moq-mux`](rs/moq-mux) | [`@moq/hang`](js/hang) |
| Capture, codecs & playback | [`moq-video`](rs/moq-video), [`moq-audio`](rs/moq-audio), [`moq-transcode`](rs/moq-transcode) | [`@moq/publish`](js/publish), [`@moq/watch`](js/watch) |
| Rooms, JSON state & archives | [`moq-json`](rs/moq-json), [`moq-archive`](rs/moq-archive) | [`@moq/room`](js/room), [`@moq/json`](js/json) |
| Authorization | [`moq-auth`](rs/moq-auth) | [`@moq/auth`](js/auth) |

**Native language bindings** (wrapping [`rs/moq-ffi`](rs/moq-ffi) via UniFFI, plus a plain C ABI; see [Layers](https://doc.moq.dev/lib/layers)):
[Swift](swift) ([docs](https://doc.moq.dev/lib/swift/)) ·
[Kotlin](kt) ([docs](https://doc.moq.dev/lib/kt/)) ·
[Python](py/moq-rs) ([docs](https://doc.moq.dev/lib/py/)) ·
[Go](go) ([docs](https://doc.moq.dev/lib/go/)) ·
[Dart](dart) ([docs](https://doc.moq.dev/lib/dart/)) ·
[C++](cpp/moq) ([docs](https://doc.moq.dev/lib/cpp/)) ·
[C (`libmoq`)](rs/libmoq) ([docs](https://doc.moq.dev/lib/c/))

## Architecture

MoQ is a small stack of independent layers.

**Rule 1**: the relay MUST NOT know anything about your application, media codecs, or even the available tracks. Everything can be end-to-end encrypted and the CDN will not care. **No business logic in the relay.**

Instead, [`moq-relay`](rs/moq-relay) routes generic live pub/sub ([`moq-lite`](https://doc.moq.dev/concept/moq-lite)) made of broadcasts, tracks, groups, and frames. Media logic lives above it in [`hang`](https://doc.moq.dev/concept/hang), which only clients and media servers understand. Extend it or replace it entirely with your own tracks.

Think of `hang` like HLS/DASH, and `moq-lite` like HTTP.

```text
┌─────────────────┐
│   Application   │   Your business logic
│                 │    - rooms, auth, AI, telemetry, game inputs
├─────────────────┤
│      hang       │   Media catalog & frame containers
│                 │    - codecs, timestamps, captions, JSON tracks
├─────────────────┤
│    moq-lite     │   Generic live pub/sub transport
│                 │    - broadcasts, tracks, groups, frames, datagrams
├─────────────────┤
│  WebTransport   │   Browser-compatible QUIC (with WebSocket fallback)
│      QUIC       │    - streams, prioritization, partial reliability
└─────────────────┘
```

Read [Concepts](https://doc.moq.dev/concept/) for the full walkthrough, or browse the [Internet-Drafts](https://doc.moq.dev/draft/) ([`drafts/`](drafts)) for wire specifications.

## For LLMs & Coding Agents

Building an app with MoQ using Claude Code, Cursor, Codex, or another coding agent? Paste this prompt to install the [MoQ skill](skills/moq/SKILL.md):

```text
Follow the instructions at https://doc.moq.dev/setup/agent/prompt.md
```

Or install it from your terminal:

```sh
npx -y skills add moq-dev/moq --yes
```

**Direct links for agents & automated retrieval:**

| Resource | Link | What it is |
| --- | --- | --- |
| Docs index (Markdown) | [doc.moq.dev/llms.txt](https://doc.moq.dev/llms.txt) | Concise index of every documentation page |
| Full docs bundle | [doc.moq.dev/llms-full.txt](https://doc.moq.dev/llms-full.txt) | All guides, concepts, app, and library pages in one file |
| Single doc page as Markdown | `https://doc.moq.dev/<path>.md` | Append `.md` to any doc URL (e.g. `https://doc.moq.dev/lib/js/watch.md`) |
| Agent setup guide | [doc.moq.dev/setup/agent](https://doc.moq.dev/setup/agent) | App-building skill and repo workflow details |
| Repository instructions | [AGENTS.md](AGENTS.md) | Root rules and cross-package sync requirements (check nested `AGENTS.md` files too) |
| Repository map | [doc/setup/repository.md](doc/setup/repository.md) | Maps every feature to its crates, packages, tests, and docs |
| Runnable examples | [doc/setup/examples.md](doc/setup/examples.md), [`demo/`](demo) | Working code to inspect instead of guessing APIs |
| Wire specifications | [`drafts/`](drafts) | Editable Internet-Draft sources for `moq-lite`, `hang`, and extensions |

## Stability & Development

**Stability**: `release` is the stable branch that gets published; `main` is the development trunk where breaking API changes land between releases. Every package follows [semver](https://semver.org) (a breaking API change bumps the first nonzero version number, e.g. `1.2.0` to `2.0.0` or `0.3.1` to `0.4.0`), and breaking wire changes are rare. See [Upgrade](https://doc.moq.dev/setup/upgrade) for migration guides.

**Contributing**: Contributions are welcome, and AI-assisted issues, pull requests, reviews, and comments are encouraged; see [CONTRIBUTING.md](CONTRIBUTING.md#ai). Run commands inside `nix develop` so tool versions match CI:

```sh
just --list      # see all available commands
just             # start the local relay, demo media, and web server
just check       # lint, compile, and test what your branch changed
just fix         # auto-fix formatting and lints (same scope)
just ci          # run the merge queue's gate locally before pushing
```

See the [development guide](doc/setup/dev.md), [troubleshooting](https://doc.moq.dev/setup/troubleshooting), and [justfile](justfile) for full-repo checks (`--all`, `just gate`), cross-language interop tests, and Windows setup.

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

**Exception:** the OBS plugin under [`cpp/obs/`](cpp/obs) is licensed under **GPL-2.0-or-later** (see [`cpp/obs/LICENSE`](cpp/obs/LICENSE)), because it links OBS Studio's `libobs`, which is GPL-2.0. This is a separately-distributable work; per GPLv2 its presence in this repository is mere aggregation and does not affect the MIT/Apache licensing of the rest of the project. `libmoq` and the other moq crates remain MIT/Apache.
