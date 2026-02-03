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

## Repository
The plugin is maintained in a separate repository:

**GitHub:** [moq-dev/gstreamer](https://github.com/moq-dev/gstreamer)

## Usage
See the [Justfile](https://github.com/moq-dev/gstreamer/blob/main/justfile) for more complex and up-to-date examples.

```bash
gst-launch-1.0 videotestsrc ! x264enc ! isofmp4mux name=mux chunk-duration=1 fragment-duration=1 ! moqsink url=https://cdn.moq.dev/anon broadcast=test
```

## Subscribing
```bash
gst-launch-1.0 moqsrc url=https://cdn.moq.dev/anon broadcast=test ! decodebin ! autovideosink
```
