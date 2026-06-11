---
title: Interoperability
description: Point moq-cli at any MoQ relay to publish and subscribe
---

# Interoperability

This code speaks two wire protocols and negotiates one of them at connect time:

- [moq-lite](/concept/layer/moq-lite), our simplified protocol.
- The full IETF `moq-transport` draft.

[moq-lite](/concept/layer/moq-lite) is a forwards-compatible subset of moq-transport,
so a moq-lite client can talk to any moq-transport relay (but not necessarily the reverse).
You don't have to pick: the client offers both and lets the relay choose.

The fastest way to interop is `moq-cli`. Point it at your relay (or a third-party
relay) and publish or subscribe. The rest of this page covers installing it,
the protocols it negotiates, and the commands to run.

## Transport

### Versions

The client negotiates via ALPN, offering moq-lite and moq-transport drafts 14
through 18. If the relay doesn't select a version through ALPN, the client falls
back to draft-14 framing and negotiates the version after setup.

- moq-transport-14
- moq-transport-15
- moq-transport-16
- moq-transport-17
- moq-transport-18
- moq-lite

The ALPN list the client offers, in preference order:

```
moq-lite-04, moq-lite-03, moql, moqt-18, moqt-17, moqt-16, moqt-15, moq-00
```

### Protocols

- WebTransport (over HTTP/3)
- QMux over WebSocket
- (Rust) raw QUIC
- (Rust) QMux over TLS

### TLS

WebTransport and QUIC both require TLS. If the relay uses a self-signed or
expired certificate (common for interop sandboxes), disable verification:

```bash
moq-cli subscribe --tls-disable-verify --url https://relay.example.com ...
```

## Install the client

`moq-cli` reads media from stdin (or writes it to stdout) and exchanges it with
a relay. It pairs with FFmpeg for encoding and decoding.

```bash
# macOS / Linux: Homebrew
brew install moq-dev/tap/moq-cli

# Any platform with a Rust toolchain
cargo install moq-cli

# Docker / Podman (linux/amd64 + linux/arm64)
docker pull moqdev/moq-cli            # or: podman pull moqdev/moq-cli
docker run --rm moqdev/moq-cli --version
```

Debian/Ubuntu and Fedora/RHEL have native packages from `apt.moq.dev` and
`rpm.moq.dev`. See [Linux Packages](/setup/linux) for the repository setup.

To run an unreleased build straight from a checkout:

```bash
nix run github:moq-dev/moq#moq-cli -- subscribe --url https://relay.example.com ...
# or
cargo run -p moq-cli -- subscribe --url https://relay.example.com ...
```

You also need FFmpeg (`brew install ffmpeg`, `apt install ffmpeg`) for the
encode/decode steps below.

## Publish to a relay

Pipe an encoded stream into `moq-cli publish`. The input container is selected
by a subcommand; the broadcast name carries a `.hang` suffix so the catalog
format is explicit.

```bash
# Fragmented MP4 / CMAF
ffmpeg -re -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -c:a aac \
    -f mp4 -movflags cmaf+frag_keyframe+empty_moov+default_base_moof - \
| moq-cli publish --url https://relay.example.com --broadcast my-stream.hang fmp4
```

```bash
# MPEG-TS (remux without re-encoding)
ffmpeg -re -i input.mp4 -c copy -f mpegts - \
| moq-cli publish --url https://relay.example.com --broadcast my-stream.hang ts
```

Input containers (`publish` subcommand, read from stdin unless noted):

- `avc3` - raw H.264 Annex-B
- `fmp4` - fragmented MP4 / CMAF
- `ts` - MPEG-TS (H.264 / H.265 video, AAC audio)
- `hls --playlist <url-or-path>` - ingest an HLS playlist (does not read stdin)

## Subscribe from a relay

`moq-cli subscribe` pulls a broadcast and writes a container to stdout. Select
the output container with `--format`, then play it with FFmpeg.

```bash
moq-cli subscribe --url https://relay.example.com --broadcast my-stream.hang --format fmp4 | ffplay -
```

Output containers (`--format`):

- `fmp4` - fragmented MP4 / CMAF
- `mkv` - Matroska / WebM
- `ts` - MPEG-TS

## Announce discovery and ordering

The publisher sends a `PUBLISH_NAMESPACE` (announce); the subscriber sends a
`SUBSCRIBE_NAMESPACE` and waits for a matching announce before it subscribes to
any track. This means **the subscriber must learn about the broadcast through an
announce**, which has an ordering implication on some relays:

- A relay that **replays** current announcements (ours does) delivers the
  announce whether the subscriber joins before or after the publisher. Order
  doesn't matter.
- A relay that does **not** replay (some third-party relays) only forwards an
  announce live. A subscriber that connects after the publisher already
  announced will sit idle and receive nothing. Start the subscriber first, then
  the publisher.

If your subscriber doesn't implement `SUBSCRIBE_NAMESPACE` yet, the web demo
can subscribe to a known name directly: remove `reload` in
`demo/web/src/index.html`.

## URL paths and broadcast names

WebTransport prepends the URL path to the broadcast name. A relay that serves a
path like `/anon` turns broadcast `my-stream` into `/anon/my-stream` on the
wire, so connect to `https://relay.example.com/anon` and publish `my-stream`.
Raw QUIC and iroh have no HTTP layer, so include the prefix in the broadcast
name yourself (e.g. `/anon/my-stream`).

## Authentication

Pass a JWT via the URL query string:

```bash
moq-cli publish --url "https://relay.example.com/room/123?jwt=<token>" --broadcast my-stream.hang fmp4
```

See [Authentication](/bin/relay/auth) for generating tokens.

## Debugging

```bash
# Verbose protocol logs (connect, version, announce, subscribe)
RUST_LOG=info,moq_net=debug moq-cli subscribe --url https://relay.example.com --broadcast my-stream.hang --format fmp4 > /dev/null
```

The log prints the negotiated version (e.g. `connected version=moq-transport-18`)
and each `announce` / `subscribe started` event, which is usually enough to tell
whether the relay is forwarding your broadcast.

## Other clients

`moq-cli` is the quickest, but not the only option:

- [GStreamer](/bin/gstreamer) (`moqsink` / `moqsrc`) and [OBS](/bin/obs) both
  publish and subscribe.
- The browser stack ([web components](/lib/js/env/web)) subscribes and publishes
  over WebTransport.
- There are native bindings for Rust, TypeScript, C, Python, Kotlin, Swift, and Go.

## Media

We primarily use [hang](/concept/layer/hang), a catalog plus container format
(think a mix of MSF and LOC). The relay itself is media-agnostic, so this only
matters for a client that needs to decode the payload.

The Rust publisher writes two catalog tracks pointing at the same media tracks:

- `catalog.json` ([hang](/concept/layer/hang))
- `catalog` ([MSF](/concept/standard/msf))

Two containers are supported, though `cmaf` is still experimental:

- `legacy` ([hang](/concept/layer/hang))
- `cmaf` ([CMAF](/concept/standard/cmaf))

The JavaScript code currently only produces `catalog.json`, but its subscriber
consumes both `legacy` and `cmaf` containers.

## Testing against our relay

Our relay implements a deliberately shallow subset of the moq-transport draft,
so it's more useful as a publish/subscribe target than as a strict conformance
peer. Run one locally (also available at `https://cdn.moq.dev/anon`):

```bash
just relay
```

It currently **ignores** the following:

- Any sub-group >0
- Any datagrams
- Any FETCH, except a JOINING FETCH, which is a no-op
- Any objects with delta >0 (must be contiguous)
- Any object properties
- Any SUBSCRIBE `forward=0`
- Any multi-publisher behavior
- Probably some other things

All subscriptions start at the latest group.
