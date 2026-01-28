---
title: OBS Plugin
description: OBS Studio plugin for MoQ
---

# OBS Plugin

An OBS Studio plugin for publishing and consuming MoQ streams.

::: warning Work in Progress
This plugin is currently under development.
:::

## Overview

The OBS plugin allows you to:
- **Publish** directly from OBS to a MoQ relay
- **Subscribe** to MoQ broadcasts as an OBS source

## Repository

The plugin is maintained in a separate repository:

**GitHub:** [moq-dev/obs](https://github.com/moq-dev/obs)

## Installation

Instructions coming soon. The plugin will be available for:
- Windows
- macOS
- Linux

## Usage

### Publishing

1. Open OBS Studio
2. Go to Settings > Stream
3. Select "MoQ" as the service
4. Enter your relay URL and path
5. Click "Start Streaming"

### Subscribing

1. Add a new source
2. Select "MoQ Source"
3. Enter the relay URL and broadcast path
4. The stream will appear in your scene

## Configuration

Configuration options will include:
- Relay URL
- Broadcast path
- Authentication (JWT token)
- Video codec (H.264, H.265, AV1)
- Audio codec (Opus, AAC)
- Bitrate settings

## Next Steps

- Check the [GitHub repository](https://github.com/moq-dev/obs) for updates
- Use [hang-cli](/ffmpeg/) for command-line publishing
- Deploy a [relay server](/relay/)
