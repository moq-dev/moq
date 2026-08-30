---
title: Quick Start
description: Run the MoQ demo locally
---

# Quick Start

The default demo starts a relay, publishes a test video, and opens the web client.
Everything runs on your machine.

First, clone the repository:

```bash
git clone https://github.com/moq-dev/moq
cd moq
```

## With Nix

Nix is the recommended setup because it uses the tool versions pinned by the
repository. Install [Nix](https://nixos.org/download.html) with flakes enabled,
then run:

```bash
nix develop --command just
```

With [nix-direnv](https://github.com/nix-community/nix-direnv), entering the
repository loads the development shell and the command becomes `just`.

## Without Nix

Install these tools first:

- [Just](https://github.com/casey/just)
- [Rust](https://www.rust-lang.org/tools/install)
- [Bun](https://bun.sh/)
- [FFmpeg](https://ffmpeg.org/download.html)

Then install the workspace tools and start the demo:

```bash
just install
just
```

Some optional targets need additional system libraries. GStreamer development
packages are required for `moq-gst`; a C toolchain is required for `libmoq`;
and the language bindings need their respective toolchains. The Nix development
shell includes these dependencies.

Windows users should follow the [Windows setup](/setup/windows). Linux packages
for running released binaries are covered in the [Linux guide](/setup/linux).

## What starts

The default recipe runs three components:

1. [moq-relay](/bin/relay/) routes broadcasts between publishers and subscribers.
2. [moq-cli](/bin/cli) publishes a test video through the relay.
3. The [web demo](/setup/demo/web) opens at [localhost:5173](http://localhost:5173).

The local relay uses a generated certificate and fingerprint verification. A
public deployment needs a stable hostname, trusted TLS certificate, and an open
UDP port. See [production deployment](/setup/prod).

## Next steps

- Use the [development guide](/setup/dev) for tests, debugging, and more demos.
- Try publishing with [OBS](/bin/obs) or [GStreamer](/bin/gstreamer).
- Explore the [applications](/bin/) and [libraries](/lib/).
