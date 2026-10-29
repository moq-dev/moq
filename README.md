<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)
[![Discord](https://img.shields.io/discord/1124083992740761730)](https://discord.moq.dev)
[![Crates.io](https://img.shields.io/crates/v/moq-net)](https://crates.io/crates/moq-net)
[![npm](https://img.shields.io/npm/v/@moq/net)](https://www.npmjs.com/package/@moq/net)

# Media over QUIC

[Media over QUIC](https://moq.dev) (MoQ) is a next-generation live media protocol that provides **real-time latency** at **massive scale**.
Built using modern web technologies, MoQ delivers WebRTC-like latency without the constraints of WebRTC.
The core networking is delegated to a QUIC library but the rest is in application-space, giving you full control over your media pipeline.

**Key Features:**

- 🚀 **Real-time latency** using QUIC for prioritization and partial reliability.
- 📈 **Massive scale** designed for fan-out and supports cross-region clustering.
- 🌐 **Modern Web** using [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API), [WebCodecs](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API), and [WebAudio](https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API).
- 🎯 **Multi-language** with both Rust (native) and TypeScript (web) libraries.
- 🔧 **Generic** for any live data, not just media. Includes text chat as both an example and a core feature.

> **Note:** This project implements [moq-lite](https://doc.moq.dev/concept/moq-lite), a forwards-compatible subset of the IETF [moq-transport](https://datatracker.ietf.org/doc/draft-ietf-moq-transport/) draft. moq-lite works with any moq-transport CDN (ex. [Cloudflare](https://moq.dev/blog/first-cdn/)). The focus is narrower, prioritizing simplicity and deployability.

## Getting Started

Full documentation lives at **[doc.moq.dev](https://doc.moq.dev)**.

| I want to | Start here |
| --- | --- |
| Try MoQ | [Local demo](https://doc.moq.dev/setup/) or [live web demo](https://moq.dev/) |
| Add streaming to an app | [Choose a library](https://doc.moq.dev/lib/) and [find an example](https://doc.moq.dev/setup/examples) |
| Connect existing media tools | [CLI, OBS, GStreamer, and gateways](https://doc.moq.dev/bin/) |
| Run a relay | [Install](https://doc.moq.dev/setup/install), then [production setup](https://doc.moq.dev/setup/prod) |
| Understand the design | [Concepts](https://doc.moq.dev/concept/) and [protocol drafts](https://doc.moq.dev/draft/) |
| Diagnose a problem | [Troubleshooting](https://doc.moq.dev/setup/troubleshooting) and [inspect a relay](https://doc.moq.dev/bin/inspect) |
| Change this repository | [Repository map](doc/setup/repository.md), [development guide](doc/setup/dev.md), and [contributing](CONTRIBUTING.md) |
| Work with a coding agent | [Agent setup](doc/setup/agent.md); repository instructions start in [AGENTS.md](AGENTS.md) |

The quickest way to see it in action (requires [Nix](https://nixos.org/download.html) with [flakes](https://wiki.nixos.org/wiki/Flakes)):

```sh
# Runs a relay, demo media, and the web server
nix develop -c just
```

It opens the watch, publish, and stats pages in your browser, or visit <http://localhost:5173/watch.html>. Don't have Nix? See the [demo guide](https://doc.moq.dev/setup/) for manual setup.

## Architecture

MoQ is designed as a layered protocol stack.

**Rule 1**: The CDN MUST NOT know anything about your application, media codecs, or even the available tracks.
Everything could be fully E2EE and the CDN wouldn't care. **No business logic allowed**.

Instead, [`moq-relay`](rs/moq-relay) operates on rules encoded in the [`moq-net`](https://docs.rs/moq-net) header.
These rules are based on video encoding but are generic enough to be used for any live data.
The goal is to keep the server as dumb as possible while supporting a wide range of use-cases.

The media logic is split into another protocol called [`hang`](https://docs.rs/hang).
It's pretty simple and only intended to be used by clients or media servers.
If you want to do something more custom, then you can always extend it or replace it entirely.

Think of `hang` as like HLS/DASH, while `moq-lite` is like HTTP.

```
┌─────────────────┐
│   Application   │   🏢 Your business logic
│                 │    - authentication, non-media tracks, etc.
├─────────────────┤
│      hang       │   🎬 Media-specific encoding/streaming
│                 │     - codecs, containers, catalog
├─────────────────├
│    moq-lite     │  🚌 Generic pub/sub transport
│                 │     - broadcasts, tracks, groups, frames
├─────────────────┤
│  WebTransport   │  🌐 Browser-compatible QUIC
│      QUIC       │     - HTTP/3 handshake, multiplexing, etc.
└─────────────────┘
```

## Libraries

Choose by the job you need done. The [library guide](https://doc.moq.dev/lib/)
compares languages; the [repository map](doc/setup/repository.md) locates the
implementation behind each feature.

| Layer | Rust | TypeScript |
| --- | --- | --- |
| Live pub/sub | [moq-net](rs/moq-net) | [@moq/net](js/net) |
| Media catalog and containers | [hang](rs/hang), [moq-mux](rs/moq-mux) | [@moq/hang](js/hang) |
| Capture and playback | [moq-audio](rs/moq-audio), [moq-video](rs/moq-video) | [@moq/publish](js/publish), [@moq/watch](js/watch) |
| Rooms and chat | [moq-room](rs/moq-room) | [@moq/room](js/room) |
| Authorization | [moq-auth](rs/moq-auth) | [@moq/auth](js/auth) |

The Rust core also has [Python](https://doc.moq.dev/lib/py/),
[Swift](https://doc.moq.dev/lib/swift/), [Kotlin](https://doc.moq.dev/lib/kt/),
[Go](https://doc.moq.dev/lib/go/), [Dart](https://doc.moq.dev/lib/dart/),
[C++](https://doc.moq.dev/lib/cpp/), and [C](https://doc.moq.dev/lib/c/) bindings.
[Layers](https://doc.moq.dev/lib/layers) maps their namespaces.

## Protocol

Start with the [layering overview](https://doc.moq.dev/concept/), then use the
[Internet-Drafts index](https://doc.moq.dev/draft/) for the wire and format
specifications. Their editable sources live in [drafts/](drafts/).

## Stability

The `release` branch is stable and is what gets published; `main` is the trunk where breaking API changes land between releases.
Every package follows [semver](https://semver.org): a breaking API change bumps the first nonzero version number (`1.2.0` to `2.0.0`, `0.3.1` to `0.4.0`, `0.0.5` to `0.0.6`).
Breaking wire changes are extremely rare.

## Development

Contributions are welcome, and AI-assisted issues, pull requests, reviews, and comments are encouraged. See [CONTRIBUTING.md](CONTRIBUTING.md#ai) for the attribution policy and guidance on when to open an issue before writing code.

```sh
# See all available commands
just --list

# Build everything
just build

# Lint, compile, and test what your branch changed
just check

# Automatically fix some linting errors, same scope
just fix

# Same as the above, over every package
just check --all
just fix --all
```

Expensive checks and test launchers automatically enter `quest gate` locally,
including direct language recipes and fixture scripts. Admission waits for host
CPU load and available memory before builds or test timeout clocks start;
nested checks reuse that admission. Hosted GitHub Actions keeps its runner
scheduling. Rust builds still use the dev shell's mbx Cargo shim.

`just check` and `just ci` run three module lanes at once, each admitted per
command; set `CHECK_JOBS` to change that, or to 1 to run them in order.
Local nextest runs default to at most four test workers; set
`NEXTEST_TEST_THREADS` to override it. Bun workspace scripts run sequentially
locally, preserving dependency order, while CI keeps its workspace parallelism.
Selection and an empty JS selection do not wait for admission. For a build
outside these entrypoints, use `quest gate COMMAND [ARGS...]`; gate thresholds
come from the host and have no configuration.

PRs into `main` get no cloud CI until the merge queue, so run `just ci` before pushing. It runs what the queue runs, the same ground as `just check`, plus two settings: `CI_STRICT=1`, which turns a missing tool into an error instead of a skipped check, and `NEXTEST_PROFILE=ci`, which allows a longer hang timeout.

See the [development guide](https://doc.moq.dev/setup/dev) and the [justfile](justfile) for more.

## License

Licensed under either:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

**Exception:** the OBS plugin under [`cpp/obs/`](cpp/obs) is licensed under **GPL-2.0-or-later** (see [`cpp/obs/LICENSE`](cpp/obs/LICENSE)), because it links OBS Studio's `libobs`, which is GPL-2.0. This is a separately-distributable work; per GPLv2 its presence in this repository is mere aggregation and does not affect the MIT/Apache licensing of the rest of the project. `moq-c` and the other moq crates remain MIT/Apache.
