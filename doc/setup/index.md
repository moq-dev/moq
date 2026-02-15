---
title: Quick Start
description: Get started with MoQ in seconds
---

# Quick Start

We've got a few demos to show off MoQ in action.
Everything runs on localhost in development, but in production of course you'll run these components across multiple hosts.

Start by cloning the repo:
```bash
git clone https://github.com/moq-dev/moq
cd moq
```

Then pick your poison: Nix or not.

## Option 1: Using Nix (Recommended)

The recommended approach is to use [Nix](https://nixos.org/download.html).
It's like Docker but without the VM; all dependencies are pinned to specific versions.

Install the following:
- [Nix](https://nixos.org/download.html)
- [Nix Flakes](https://nixos.wiki/wiki/Flakes)
- (optional) [Nix Direnv](https://github.com/nix-community/nix-direnv)

Then run the demo:
```bash
# Runs the demo using pinned dependencies
nix develop -c just dev
```

If you install `direnv`, then the Nix shell will be loaded whenever you `cd` into the repo:
```bash
# Run the demo... in 9 keystrokes
just dev
```


## Option 2: Manual Installation

If you don't like Nix or enjoy suffering with Windows, then you can manually install the dependencies:

- [Just](https://github.com/casey/just)
- [Rust](https://www.rust-lang.org/tools/install)
- [Bun](https://bun.sh/)
- [FFmpeg](https://ffmpeg.org/download.html)
- ...more?

Then run:
```bash
# Install additional dependencies, usually linters
just install

# Run the demo
just dev
```

When in doubt, check the [Nix Flake](https://github.com/moq-dev/moq/blob/main/flake.nix) for the full list of dependencies.

## What's Happening?
The `just dev` command starts three components:

- [moq-relay](/app/relay/): A server that routes live data between publishers and subscribers.
- [moq-cli](/app/cli): A CLI that publishes video content piped from `ffmpeg`.
- [demo](/js/@moq/demo): A web page with various demos.

Once everything compiles, it should open [localhost:5173](http://localhost:5173) in your browser.

::: warning
The demo uses an insecure HTTP fetch for local development only. In production, you'll need a proper domain and TLS certificate via [LetsEncrypt](https://letsencrypt.org/docs/) or similar.
:::

### More Demos
Check out the full [development guide](/setup/dev) for more demos and commands.

We have separate repos for the more difficult plugins to compile:
- [OBS](/app/obs)
- [Gstreamer](/app/gstreamer)
