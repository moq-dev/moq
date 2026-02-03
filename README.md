<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue)
[![Discord](https://img.shields.io/discord/1124083992740761730)](https://discord.gg/FCYF3p99mr)
[![Crates.io](https://img.shields.io/crates/v/moq-lite)](https://crates.io/crates/moq-lite)
[![npm](https://img.shields.io/npm/v/@moq/lite)](https://www.npmjs.com/package/@moq/lite)

# Media over QUIC

[Media over QUIC](https://moq.dev) (MoQ) is a next-generation live media protocol that provides **real-time latency** at **massive scale**.
Built using modern web technologies, MoQ delivers WebRTC-like latency without the constraints of WebRTC.
The core networking is delegated to a QUIC library but the rest is in application-space, giving you full control over your media pipeline.

**Key Features:**
- 🚀 **Real-time latency** using QUIC for priotization and partial reliability.
- 📈 **Massive scale** designed for fan-out and supports cross-region clustering.
- 🌐 **Modern Web** using [WebTransport](https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API), [WebCodecs](https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API), and [WebAudio](https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API).
- 🎯 **Multi-language** with both Rust (native) and TypeScript (web) libraries.
- 🔧 **Generic** for any live data, not just media. Includes text chat as both an example and a core feature.

> **Note:** This project is a [fork](https://moq.dev/blog/transfork) of the [IETF MoQ specification](https://datatracker.ietf.org/group/moq/documents/). The focus is narrower, focusing on simplicity and deployability.


## Demo
This repository is split into multiple binaries and libraries across different languages.
It can get overwhelming, so there's an included [demo](js/hang-demo) with some examples.

**Note:** this demo uses an insecure HTTP fetch intended for *local development only*.
In production, you'll need a proper domain and a matching TLS certificate via [LetsEncrypt](https://letsencrypt.org/docs/) or similar.


### Quick Setup
**Requirements:**
- [Nix](https://nixos.org/download.html)
- [Nix Flakes enabled](https://nixos.wiki/wiki/Flakes)

```sh
# Runs a relay, demo media, and the web server
nix develop -c just dev
```

Then visit [https://localhost:8080](https://localhost:8080) to see the demo.
Note that this uses an insecure HTTP fetch for local development only; in production you'll need a proper domain + TLS certificate.

*TIP:* If you've installed [nix-direnv](https://github.com/nix-community/nix-direnv), then only `just dev` is required.


### Full Setup
If you don't like Nix, then you can install dependencies manually:

**Requirements:**
- [Just](https://github.com/casey/just)
- [Rust](https://www.rust-lang.org/tools/install)
- [Bun](https://bun.sh/)
- [FFmpeg](https://ffmpeg.org/download.html)
- ...probably some other stuff

**Run it:**
```sh
# Install some more dependencies
just install

# Runs a relay, demo media, and the web server
just dev
```

Then visit [http://localhost:5173](http://localhost:5173) to see the demo.


## Architecture

MoQ is designed as a layered protocol stack.

**Rule 1**: The CDN MUST NOT know anything about your application, media codecs, or even the available tracks.
Everything could be fully E2EE and the CDN wouldn't care. **No business logic allowed**.

Instead, [`moq-relay`](rs/moq-relay) operates on rules encoded in the [`moq-lite`](https://docs.rs/moq-lite) header.
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
This repository provides both [Rust](/rs) and [TypeScript](/js) libraries with similar APIs but language-specific optimizations.

### Rust
| Crate                       | Description                                                                                                                           | Docs                                                                           |
|-----------------------------|---------------------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------|
| [moq-lite](rs/moq-lite)          | The core pub/sub transport protocol. Has built-in concurrency and deduplication.                                                      | [![docs.rs](https://docs.rs/moq-lite/badge.svg)](https://docs.rs/moq-lite)     |
| [moq-relay](rs/moq-relay)   | A clusterable relay server. This relay performs fan-out connecting multiple clients and servers together.                             |                                                                                |
| [moq-token](rs/moq-token)   | An authentication scheme supported by `moq-relay`. Can be used as a library or as [a CLI](rs/moq-token-cli) to authenticate sessions. |                                                                                |
| [moq-native](rs/moq-native) | Opinionated helpers to configure a Quinn QUIC endpoint. It's harder than it should be.                                                | [![docs.rs](https://docs.rs/moq-native/badge.svg)](https://docs.rs/moq-native) |
| [libmoq](rs/libmoq)         | C bindings for `moq-lite`.                                                                                                            | [![docs.rs](https://docs.rs/libmoq/badge.svg)](https://docs.rs/libmoq)         |
| [hang](rs/hang)             | Media-specific encoding/streaming layered on top of `moq-lite`. Can be used as a library or [a CLI](rs/hang-cli).                     | [![docs.rs](https://docs.rs/hang/badge.svg)](https://docs.rs/hang)             |
| [hang-gst](https://github.com/moq-dev/gstreamer) | A GStreamer plugin for publishing or consuming hang broadcasts. A separate repo to avoid requiring gstreamer as a build dependency.            |                                                                                |


### TypeScript

| Package                                  | Description                                                                                                        | NPM                                                                                                   |
|------------------------------------------|--------------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------------|
| **[@moq/lite](js/lite)**             | The core pub/sub transport protocol. Intended for browsers, but can be run server-side with a WebTransport polyfill.                                   | [![npm](https://img.shields.io/npm/v/@moq/lite)](https://www.npmjs.com/package/@moq/lite)   |
| **[@moq/token](js/token)**             |  Authentication library & CLI for JS/TS environments (see [Authentication](doc/concept/authentication.md))                               | [![npm](https://img.shields.io/npm/v/@moq/token)](https://www.npmjs.com/package/@moq/token)   |
| **[@moq/hang](js/hang)**           | Media-specific encoding/streaming layered on top of `moq-lite`. Provides both a Javascript API and Web Components. | [![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang) |
| **[@moq/hang-demo](js/hang-demo)** | Examples using `@moq/hang`.                                                                                  |                                                                                                       |
| **[@moq/hang-ui](js/hang-ui)**.    | UI Components that interact with the Hang Web Components using SolidJS.                                                 | [![npm](https://img.shields.io/npm/v/@moq/hang-ui)](https://www.npmjs.com/package/@moq/hang-ui) |


## Documentation
Additional documentation and implementation details:

- **[Authentication](doc/concept/authentication.md)** - JWT tokens, authorization, and security


## Protocol
Read the specifications:
- [moq-lite](https://moq-dev.github.io/drafts/draft-lcurley-moq-lite.html)
- [hang](https://moq-dev.github.io/drafts/draft-lcurley-moq-hang.html)
- [use-cases](https://moq-dev.github.io/drafts/draft-lcurley-moq-use-cases.html)

## Development
```sh
# See all available commands
just

# Build everything
just build

# Run tests and linting
just check

# Automatically fix some linting errors
just fix

# Run the demo manually
just relay    # Terminal 1: Start relay server
just pub tos  # Terminal 2: Publish a demo video using ffmpeg
just web      # Terminal 3: Start web server
```

There are more commands: check out the [justfile](justfile), [rs/justfile](rs/justfile), and [js/justfile](js/justfile).

## Iroh support

The `moq-native` and `moq-relay` crates optionally support connecting via [iroh](https://github.com/n0-computer/iroh). The iroh integration is disabled by default, to use it enable the `iroh` feature.

When the iroh feature is enabled, you can connect to iroh endpoints with these URLs:

* `iroh://<ENDPOINT_ID>`: Connect via moq-lite over raw QUIC.
* `moql+iroh://<ENDPOINT_ID>`: Connect via moq-lite over raw QUIC (same as above)
* `moqt+iroh://<ENDPOINT_ID>`: Connect via IETF MoQ over raw QUIC
* `h3+iroh://<ENDPOINT_ID>/optional/path?with=query`: Connect via WebTransport over HTTP/3.

`ENDPOINT_ID` must be the hex-encoded iroh endpoint id. It is currently not possible to set direct addresses or iroh relay URLs. The iroh integration in moq-native uses iroh's default discovery mechanisms to discover other endpoints by their endpoint id.

You can run a demo like this:

```sh
# Terminal 1: Start a relay server
just relay --iroh-enabled
# Copy the endpoint id printed at "iroh listening"

# Terminal 2: Publish via moq-lite over raw iroh QUIC
#
# Replace ENDPOINT_ID with the relay's endpoint id.
#
# We set an `anon/` prefix to match the broadcast name the web ui expects
# Because moq-lite does not have headers if using raw QUIC, only the hostname
# in the URL can be used.
just pub bbb iroh://ENDPOINT_ID/anon --iroh-enabled
# Alternatively you can use WebTransport over HTTP/3 over iroh,
# which allows to set a path prefix in the URL:
just pub bbb h3+iroh://ENDPOINT_ID/anon --iroh-enabled

# Terminal 3: Start web server
just web
```

Then open [localhost:5173](http://localhost:5173) and watch BBB, pushed from terminal 1 via iroh to the relay running in terminal 2, from where the browser fetches it over regular WebTransport.

`just serve` serves a video via iroh alongside regular QUIC (it enables the `iroh` feature). This repo currently does not provide a native viewer, so you can't subscribe to it directly. However, you can use the [watch example from iroh-live](https://github.com/n0-computer/iroh-live/blob/main/iroh-live/examples/watch.rs) to view a video published via `hang-native`.

## License

Licensed under either:
-   Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
-   MIT license ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)
