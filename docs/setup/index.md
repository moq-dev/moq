---
title: Quick Start
description: Get started with MoQ in seconds
---

# Quick Start

We've got a few demos to show off some basic MoQ goodness.
Everything runs on localhost [in development](/setup/development), but [in production](/setup/production) you'll want to split the components across multiple hosts.

## Option 1: Using Nix (Recommended)

The recommended approach is to use [Nix](https://nixos.org/download.html).

Give it a try!
All dependencies are automatically downloaded, pinned to specific versions, and identical on CI and production.

Install the following:
- [Nix](https://nixos.org/download.html)
- [Nix Flakes](https://nixos.wiki/wiki/Flakes)

Then run the demo:
```bash
# Runs the demo
nix develop -c just dev
```

Want to make it easier? Install [nix-direnv](https://github.com/nix-community/nix-direnv), then you can simply run:

```bash
# Once only: automatically uses nix-shell inside the repository.
direnv allow

# Runs the demo
just dev
```


## Option 2: Manual Installation

If you prefer not to use Nix (or are a Windows fiend), then you can manually install the dependencies:

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

1. `moq-relay`: A server that routes live data between publishers and subscribers.
2. `hang-cli`: A tool that publishes video content, in this case the classic "Big Buck Bunny".
3. `hang-demo`: A web page with various demos, including a video player.

Once everything compiles, it should open [https://localhost:5173](localhost:5173) in your browser.
See the [development setup](/setup/development) guide for more cool things you can do.

::: warning
The demo uses an insecure HTTP fetch for local development only. In production, you'll need a proper domain and TLS certificate via [LetsEncrypt](https://letsencrypt.org/docs/) or similar.
:::

## What's Next?
If you want to run this stuff in production, you should have separate hosts for the three components.

1. `moq-relay` can be run on any host(s) with a public IP address and an open UDP port.
2. `hang-cli` can be run by any publisher client.
3. `hang-demo` can be hosted on any web server or cloud provider.

Check out the full guide for [deploying to production](/setup/production).
