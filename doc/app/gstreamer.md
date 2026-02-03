---
title: GStreamer Plugin
description: GStreamer plugin for MoQ
---

# GStreamer Plugin

A GStreamer plugin for publishing and consuming MoQ streams.

::: warning Work in Progress
This plugin is currently under development.
:::

## Overview

The GStreamer plugin provides elements for:
- **moqsrc** - Subscribe to MoQ broadcasts
- **moqsink** - Publish to MoQ relays

## Repository

The plugin is maintained in a separate repository:

**GitHub:** [moq-dev/gstreamer](https://github.com/moq-dev/gstreamer)

## Installation

Instructions coming soon. The plugin will be available for:
- Linux
- macOS
- Windows

## Usage

### Publishing

```bash
gst-launch-1.0 videotestsrc ! x264enc ! moqsink url=https://relay.example.com/anon path=test
```

### Subscribing

```bash
gst-launch-1.0 moqsrc url=https://relay.example.com/anon path=test ! decodebin ! autovideosink
```

## Pipeline Examples

### Webcam to MoQ

```bash
gst-launch-1.0 \
    v4l2src ! videoconvert ! x264enc tune=zerolatency ! \
    moqsink url=https://relay.example.com/anon path=webcam
```

### MoQ to File

```bash
gst-launch-1.0 \
    moqsrc url=https://relay.example.com/anon path=stream ! \
    decodebin ! x264enc ! mp4mux ! filesink location=output.mp4
```

### Transcoding

```bash
gst-launch-1.0 \
    moqsrc url=https://relay.example.com/anon path=input ! \
    decodebin ! videoconvert ! x264enc bitrate=1000 ! \
    moqsink url=https://relay.example.com/anon path=output-720p
```

## Configuration

Element properties will include:
- `url` - Relay URL
- `path` - Broadcast path
- `jwt` - Authentication token
- `latency` - Target latency in milliseconds

## Next Steps

- Check the [GitHub repository](https://github.com/moq-dev/gstreamer) for updates
- Use [hang-cli](/ffmpeg/) for simpler publishing
- Learn about the [protocol](/concept/protocol)
