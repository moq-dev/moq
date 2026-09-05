---
title: Quick Start
description: Run the MoQ demo locally or against the public relay
---

# Quick Start

The default demo starts a relay, publishes a test video, and opens the web
player. Everything runs on your machine.

```bash
git clone https://github.com/moq-dev/moq
cd moq
```

## With Nix (recommended)

Install [Nix](https://nixos.org/download.html) with flakes enabled, then:

```bash
nix develop --command just
```

The dev shell pins every tool the repository uses. With
[nix-direnv](https://github.com/nix-community/nix-direnv), entering the
directory loads the shell and the command becomes `just`.

## Without Nix

Install [Just](https://github.com/casey/just),
[Rust](https://www.rust-lang.org/tools/install), [Bun](https://bun.sh/), and
[FFmpeg](https://ffmpeg.org/download.html), then:

```bash
just install
just
```

Windows users should run `setup.bat` first; see [Development](/setup/dev#windows).

## What starts

1. [moq-relay](/bin/relay/) on `localhost:4443` with a generated certificate and anonymous access.
2. [moq-cli](/bin/cli) publishing Big Buck Bunny through the relay.
3. The [web demo](/bin/demo) at [localhost:5173](http://localhost:5173), where you can watch the stream or publish your camera.

## Skip the relay

A public test relay runs at `https://cdn.moq.dev/anon`. Anything published
there is public and discoverable, so pick a unique name and don't publish
private media.

```bash
# Publish a file, then open https://moq.dev/watch?name=<your-name>
ffmpeg -re -i video.mp4 -c copy -f mpegts - | \
    moq --client-connect https://cdn.moq.dev/anon --broadcast <your-name>.hang import ts
```

Every client, in every language, connects the same way: a relay URL whose path
scopes [authentication](/bin/relay/auth), plus a broadcast name. Media
broadcasts end in `.hang`.

## Next steps

- [Install](/setup/install) the relay and CLI as packages instead of building them.
- Publish from [OBS](/bin/obs), [GStreamer](/bin/gstreamer), or [RTMP/SRT/WebRTC](/bin/cli).
- Embed a player with the [web components](/lib/js/) or pick a [library](/lib/).
- [Deploy](/setup/prod) a relay with real TLS and authentication.
