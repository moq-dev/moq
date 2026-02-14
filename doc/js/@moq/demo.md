---
title: "@moq/demo"
description: Demo application showcasing MoQ in the browser
---

# @moq/demo
A demo web application that showcases MoQ capabilities.
Watch live streams, publish from your camera or screen, and explore the technology.

## Setup
Follow the [Quick Start](/setup/) guide to get started.

You can target a remote relay instead of a local one with the command:
```bash
just web https://cdn.moq.dev/anon
```

## Watch Demo
Subscribe to a live stream and play it back with adjustable latency.
The demo connects to a relay and renders video using WebCodecs.

**Features demonstrated:**
- WebTransport/WebSocket connection to relay
- Track subscription and group delivery
- WebCodecs decoding
- Latency measurement and adjustment


## Watch Demo (MSE)
The same thing as above but using MSE (Media Source Extensions) instead of WebCodecs.
The latency will be a bit higher but it'll work on more devices.


## Publish Demo
Stream your camera, microphone, or screen to the relay.
Other viewers can watch in real-time... if you publish to a remote relay.

**Features demonstrated:**
- MediaStream capture (getUserMedia, getDisplayMedia)
- WebCodecs encoding (H.264, H.265, VP8, VP9, AV1, Opus, etc)
- Catalog generation
- Track publishing
