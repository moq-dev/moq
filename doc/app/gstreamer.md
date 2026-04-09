---
title: GStreamer Plugin
description: GStreamer plugin for MoQ
---

# GStreamer Plugin

A GStreamer plugin for publishing and consuming MoQ streams.

::: warning Work in Progress
This plugin is currently under development, but it works okay.
:::

## Overview

The GStreamer plugin provides elements for:

- **moqsrc** - Subscribe to MoQ broadcasts
- **moqsink** - Publish to MoQ relays

## Setup

The plugin lives in the monorepo at `rs/moq-gst` but is not built by default since it requires GStreamer development libraries.

If you're using Nix, GStreamer is included in the dev shell automatically. Otherwise, install GStreamer and its development packages manually:

- **macOS:** `brew install gstreamer`
- **Debian/Ubuntu:** `apt install libgstreamer1.0-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad`

Then build the plugin:

```bash
cargo build -p moq-gst
```

## Usage

```bash
gst-launch-1.0 videotestsrc ! x264enc ! isofmp4mux name=mux chunk-duration=1 fragment-duration=1 ! moqsink url=https://cdn.moq.dev/anon broadcast=test
```

## Subscribing

```bash
gst-launch-1.0 moqsrc url=https://cdn.moq.dev/anon broadcast=test ! decodebin ! autovideosink
```
