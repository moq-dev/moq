---
title: Applications
description: Ready-to-use tools built on MoQ
---

# Applications

These are the applications you can run today.
Some are servers, some are command-line tools, and some are web apps.


## [moq-relay](/app/relay/)
The relay server that routes broadcasts between publishers and subscribers.
This is the heart of any MoQ deployment that relies on fanout.
Run it yourself, or pay for an external service (ex. Cloudflare).

- [Configuration](/app/relay/config) - TOML reference and examples
- [Authentication](/app/relay/auth) - JWT-based access control
- [HTTP Endpoints](/app/relay/http) - Debugging and diagnostics
- [Clustering](/app/relay/cluster) - Multi-region deployment


## [moq-cli](/app/cli)
A CLI for publishing to media streams.
Another tool does the encoding (ex. ffmpeg), making it easy to pipe any media into MoQ.

```bash
# Publish your webcam
ffmpeg -f avfoundation -i "0" -f mp4 - | moq publish https://relay.example.com my-stream
```

## [OBS Plugin](/app/obs)
Real-time latency with the familiar OBS interface.
Supports both publishing and subscribing.

## [GStreamer Plugin](/app/gstreamer)
Integrate MoQ into GStreamer pipelines for advanced media workflows.
Supports both publishing and subscribing.

## [Web Demo](/app/web)
A demo web application showcasing MoQ in the browser.
Watch streams, publish from your camera, and explore the API.
