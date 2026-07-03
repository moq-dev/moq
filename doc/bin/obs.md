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

The macOS build is fully native, **not** Nix. The build spec (`cpp/obs/buildspec.json`) downloads the prebuilt obs-deps bundle (`libobs`, `Qt6`, and `ffmpeg`) on first configure, so no Homebrew packages are needed.

Requirements:

- Full **Xcode** (not just the Command Line Tools): `sudo xcode-select -s /Applications/Xcode.app`
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

The plugin statically links `libmoq`, so it ships with every libmoq release rather than on its own schedule. The [`libmoq` workflow](https://github.com/moq-dev/moq/blob/main/.github/workflows/libmoq.yml) (triggered by a `libmoq-v*` tag) rebuilds the plugin against the libmoq release it just published, then cuts a matching `obs-moq-v<version>` release with **macOS (arm64)** and **Windows (x64)** binaries. `cpp/obs/build.sh --libmoq-release <version>` drives each build (it fetches the prebuilt libmoq archive, so no second cargo build).

The archives are **unsigned**, so macOS Gatekeeper and Windows SmartScreen will warn on first load (right-click → Open on macOS). Extract the archive into your OBS plugins directory: the `.plugin` bundle on macOS, or the `obs-moq/` folder (containing `bin/64bit/` + `data/`) on Windows.

**Linux is build-from-source for now** (see the Linux section above). A prebuilt Linux binary isn't shipped: the plugin needs ffmpeg to decode subscribed video, and a Linux build links the nix/distro ffmpeg rather than the version OBS bundles, so it wouldn't load portably. (A future native decoder via `moq-video` would remove the ffmpeg dependency and let Linux ship a binary too.)

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
