---
title: OBS Plugin
description: OBS Studio plugin for MoQ
---

# OBS Plugin
An OBS Studio plugin for publishing and consuming MoQ streams.

::: warning Work in Progress
This plugin is currently under development, but works pretty gud.
:::

## Overview

The OBS plugin allows you to:
- **Publish** directly from OBS to a MoQ relay
- **Subscribe** to MoQ broadcasts as an OBS source

## Repository
The plugin is maintained in a separate repository: [moq-dev/obs](https://github.com/moq-dev/obs)

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
