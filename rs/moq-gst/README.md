<p align="center">
	<img height="128px" src="https://raw.githubusercontent.com/moq-dev/moq/main/.github/logo.svg" alt="Media over QUIC">
</p>

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-gst

A [GStreamer](https://gstreamer.freedesktop.org/) plugin for [Media over QUIC](https://moq.dev), exposing `moqsink` (and friends) as native GStreamer elements.

Uses [hang](https://github.com/moq-dev/moq/tree/main/rs/hang), [moq-mux](https://github.com/moq-dev/moq/tree/main/rs/moq-mux), and [moq-native](https://github.com/moq-dev/moq/tree/main/rs/moq-native) under the hood, so it can publish CMAF/fMP4 produced by any GStreamer pipeline directly to a MoQ relay.

This crate is not published to crates.io. Pre-built binaries are attached to GitHub releases (see below) or you can build from source.

## Install

### Debian / Ubuntu (recommended on Linux)

```bash
curl -fsSL https://apt.moq.dev/moq-keyring.gpg \
  | sudo tee /usr/share/keyrings/moq-keyring.gpg > /dev/null
echo "deb [signed-by=/usr/share/keyrings/moq-keyring.gpg] https://apt.moq.dev stable main" \
  | sudo tee /etc/apt/sources.list.d/moq.list
sudo apt update && sudo apt install gstreamer1.0-moq
```

### Fedora / RHEL / Rocky / AlmaLinux

```bash
sudo dnf config-manager --add-repo https://rpm.moq.dev/moq.repo
sudo dnf install gstreamer1-moq
```

### Nix (any platform with Nix installed)

The `moq-gst` flake output bundles the plugin with wrappers around `gst-inspect-1.0` / `gst-launch-1.0` that preload moq + the standard `gst-plugins-{base,good,bad}` set, so no `GST_PLUGIN_PATH` setup is needed.

```bash
# Inspect: list moqsink + moqsrc. (Or one-shot: `nix run github:moq-dev/moq#moq-gst -- moq`.)
nix shell github:moq-dev/moq#moq-gst --command gst-inspect-1.0 moq

# Subscribe to the always-on public test broadcast and render to a window.
nix shell github:moq-dev/moq#moq-gst --command gst-launch-1.0 -v -e \
  moqsrc url=https://cdn.moq.dev/demo broadcast=bbb.hang \
  ! decodebin3 ! videoconvert ! autovideosink

# Publish your own broadcast on the public anon relay (then sub to it from anywhere).
curl -fsSL https://vid.moq.dev/bbb.mp4 -o bbb.mp4
nix shell github:moq-dev/moq#moq-gst --command gst-launch-1.0 -v -e \
  multifilesrc location=bbb.mp4 loop=true ! parsebin name=parse \
    parse. ! queue ! identity sync=true ! mux.sink_0 \
    parse. ! queue ! identity sync=true ! mux.sink_1 \
    moqsink name=mux url=https://cdn.moq.dev/anon broadcast=my-broadcast.hang
```

See [`doc/bin/gstreamer.md`](https://github.com/moq-dev/moq/blob/main/doc/bin/gstreamer.md) for local-relay setup and audio-only variants.

### Manual install (tarball)

Download the tarball for your platform from the [releases page](https://github.com/moq-dev/moq/releases?q=moq-gst) and copy `lib/gstreamer-1.0/libgstmoq.{so,dylib}` into a directory GStreamer scans for plugins:

- **Linux**: `~/.local/share/gstreamer-1.0/plugins/` (per-user) or `/usr/lib/x86_64-linux-gnu/gstreamer-1.0/` (system-wide). Alternatively, `export GST_PLUGIN_PATH=/path/to/dir`.
- **macOS**: `~/Library/Application Support/GStreamer/1.0/plugins/`. Alternatively, `export GST_PLUGIN_PATH=/path/to/dir`.

Then verify:

```bash
gst-inspect-1.0 moq
```

You should see `moqsink` and `moqsrc` listed.

Available targets:

| target | platform |
|---|---|
| `x86_64-unknown-linux-gnu` | Linux (x86\_64) |
| `aarch64-unknown-linux-gnu` | Linux (ARM64) |
| `x86_64-apple-darwin` | macOS (Intel) |
| `aarch64-apple-darwin` | macOS (Apple Silicon) |

You need a matching GStreamer runtime installed:

- **Debian/Ubuntu**: `sudo apt install gstreamer1.0-plugins-base gstreamer1.0-plugins-good` (1.24+)
- **macOS (Homebrew)**: `brew install gstreamer`
- **macOS (official)**: the .pkg installer from [gstreamer.freedesktop.org](https://gstreamer.freedesktop.org/download/)

The macOS dylib has rpaths pre-baked for the three common install locations (`/opt/homebrew/lib`, `/usr/local/lib`, `/Library/Frameworks/GStreamer.framework/Libraries`). If your GStreamer is somewhere else, set `DYLD_FALLBACK_LIBRARY_PATH` accordingly.

## Building from source

```bash
cargo build --release -p moq-gst
```

The resulting plugin is at `target/release/libgstmoq.so` (or `.dylib` / `.dll`). Point `GST_PLUGIN_PATH` at the containing directory to make it discoverable.

For a reproducible build matching the release artifacts (Linux + macOS):

```bash
nix build .#moq-gst
ls result/lib/gstreamer-1.0/
```
