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

Build it when you want to *run* it. To check that a change compiles, reach for [`just obs compile`](#type-checking) instead: on macOS and Windows a real build first downloads the multi-hundred-MB obs-deps bundle into the tree it's building in, which is per-worktree.

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

### Type-checking

`just obs compile` type-checks every plugin source without linking, and without downloading anything:

```bash
nix develop
just obs compile
```

This is the gate to run while working, because it needs headers rather than libraries, and the dev shell carries all of them on every platform. `libobs` comes from the `libobs-headers` package in `flake.nix`, which unpacks the headers from the same OBS release `buildspec.json` pins (`just obs check` fails if the two versions drift); `Qt6` and `ffmpeg` come from nixpkgs. It regenerates `target/include/moq.h` first, so a call to a `libmoq` function whose signature has since changed is a compile error rather than something you find out about later.

It compiles the Qt sources too, which the CMake build only does when `ENABLE_QT` and `ENABLE_FRONTEND_API` are on. `just check` runs it for you when a branch touches `cpp/obs/` or `rs/libmoq/`.

### Compiling in CI

[`obs.yml`](https://github.com/moq-dev/moq/blob/main/.github/workflows/obs.yml) compiles **and links** the plugin on every PR that touches the plugin, `rs/libmoq/`, a workspace manifest or build script, or the flake. It runs on Linux, the one platform where the whole dependency set (`libobs`, `Qt6`, `ffmpeg`) comes from nixpkgs with no obs-deps bundle to download. The plugin is platform-independent C++ over libmoq's C ABI, so this catches what a macOS developer would otherwise ship uncompiled. `just obs ci` is the same recipe locally.

The filter reaches past `cpp/obs/` because this is the only place `libmoq.a` is linked from outside cargo, which needs the hand-maintained native-library lists in `rs/libmoq/native-libs/`. A dependency that starts pulling in a new native library leaves those stale, and every Rust gate stays green because cargo passes the flag itself. No list of paths catches all of those, so [`nightly.yml`](https://github.com/moq-dev/moq/blob/main/.github/workflows/nightly.yml) runs the same recipe diff-independently as the backstop.

### Tests

`just obs test` compiles the plugin sources against stubbed `libobs`/`libmoq` under ThreadSanitizer and drives the session status callback's orderings directly: a connection that fails permanently, a terminal arriving mid-`Start()`, a restart, and one arriving while the output is being destroyed. Run it after touching `cpp/obs/src/`.

It finds the `libobs` headers the same way `just obs compile` does, and regenerates `moq.h` the same way; set `OBS_INCLUDE_DIR` to point it somewhere else. That shared step asks cargo where the header landed and reads the answer with `jq`, so outside the dev shell (running from WSL, say) `jq` has to be installed alongside cargo and the compiler. It stays a manual gate, like `just rs macos`: CI links the plugin but doesn't run these, since ThreadSanitizer needs its own build.

It also needs a Clang or GCC whose ThreadSanitizer runtime *runs* on the host, so it fails rather than skipping when one isn't available. On Windows run it from WSL, since neither MSVC nor Clang on Windows implements ThreadSanitizer.

```bash
just obs test
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

### Advanced settings

The defaults are what you want for streaming to a normal relay. The advanced settings
exist for testing against a specific protocol draft, reaching a relay with a self-signed
certificate, and diagnosing a connection that misbehaves.

They live in two places, backed by the same values:

- **Settings > Stream**, under the collapsible **Advanced** group. Saved with the rest of
  the service, so they travel with the profile.
- **The MoQ dock**, via the **Advanced…** button, which opens them in their own window so
  the dock stays small.

Everything is ignored unless the group is switched on. With the group off, the plugin
connects with the libmoq defaults. If a value is rejected (an unknown version, an
unparseable bind address), the stream refuses to start and the log records which setting
was rejected and why.

| Setting | What it's for |
| --- | --- |
| Protocol version | Pin the handshake to one draft instead of offering all of them. The menu lists what this build offers; a work-in-progress draft can be typed in. |
| QUIC backend | Pick one of the backends compiled into this libmoq build instead of its default. |
| Bind address | Send from a specific local address, e.g. `192.0.2.7:0` to pin the outgoing interface. |
| Connect timeout | Bound on one attempt, dial and handshake together. `0` waits forever. |
| Happy Eyeballs delay | How long before also trying the next address DNS returned. |
| Skip certificate verification | Development only: accepts any certificate. Prefer a fingerprint. |
| Certificate fingerprint | Trust one self-signed certificate by its SHA-256 hex fingerprint, the native equivalent of the browser's `serverCertificateHashes`. |
| Root certificate | Trust a PEM CA instead of the system roots. |
| Server name override | Validate against this name instead of the URL host, so a relay can be reached by IP. |
| Reconnect delay / cap / give up after | Retry pacing after a drop. "Give up after" is also how long the broadcast lingers for viewers across the gap; `0` retries forever. |
| Congestion control | Delay-based (BBR) keeps queues short and the send rate steady enough for an encoder to track. Loss-based (CUBIC) chases throughput. |
| Max concurrent streams | MoQ opens a stream per group, so a busy publisher wants this high. |
| Idle timeout / Keep-alive | Connection liveness. A keep-alive of `0` disables the pings. |
| UDP segmentation offload | Batches sends into one syscall. Turn it off if large sends vanish; some NICs and middleboxes mangle segmented packets. |
| Path MTU discovery | Leave it automatic for the library's choice, or explicitly enable or disable it. |
| qlog directory | On builds with qlog support, write QUIC connection traces here for diagnosing stalls. The files get large. |
| WebSocket fallback (+ delay) | Race a WebSocket connection against QUIC so a network that blocks UDP still goes live. Turn it off to measure the QUIC path alone. |
