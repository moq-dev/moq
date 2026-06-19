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

It loads into a stock OBS Studio install. You no longer need to build OBS from source to use it.

## Building

The plugin lives in-tree under `cpp/obs/`. It links `libmoq`, which is built from the in-tree `rs/libmoq` crate via cargo (CMake's `MOQ_LOCAL` points at the repo root by default), so there is no prebuilt release to download.

### Linux (Nix)

`libobs`, `Qt6`, and `ffmpeg` come from the dev shell; no system packages required.

```bash
nix develop
just obs build
```

### macOS

The macOS build is fully native, **not** Nix. The build spec (`cpp/obs/buildspec.json`) downloads prebuilt `libobs` and `Qt6` on first configure, but `ffmpeg` and `pkg-config` come from Homebrew.

Requirements:

- Full **Xcode** (not just the Command Line Tools): `sudo xcode-select -s /Applications/Xcode.app`
- `brew install ffmpeg pkg-config`
- Run **outside** the Nix dev shell. The Nix toolchain sets `DEVELOPER_DIR`/`NIX_LDFLAGS`, which break the Xcode build. If you use direnv, run from a plain terminal or `exit` the shell first.

```bash
just obs setup   # downloads obs-deps, configures via the macOS preset
just obs build
just obs run     # copies the plugin into ~/Library/Application Support/obs-studio/plugins and launches OBS
```

### Windows

Needs Visual Studio 2022. Run from Git Bash (for `just`); the build spec downloads obs-deps the same way as macOS.

```bash
just obs setup
just obs build
```

## Releases

Pushing an `obs-moq-v*` tag runs [`.github/workflows/obs.yml`](https://github.com/moq-dev/moq/blob/main/.github/workflows/obs.yml): it builds on Linux (x86_64 + arm64, via Nix), macOS (arm64), and Windows (x64), then attaches per-platform archives to a GitHub release. `cpp/obs/build.sh` drives the per-platform build and packaging.

The archives are **unsigned**, so macOS Gatekeeper and Windows SmartScreen will warn on first load (right-click → Open on macOS). Extract the archive into your OBS plugins directory: the `.plugin` bundle on macOS, or the `obs-moq/` folder (containing `bin/64bit/` + `data/`) on Linux/Windows.

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
