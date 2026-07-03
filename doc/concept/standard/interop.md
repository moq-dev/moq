---
title: Interoperability
description: Publish and subscribe to a moq-transport relay with moq-cli
---

# Interoperability

`moq-cli` speaks moq-transport drafts **14 through 18**, negotiated over ALPN at
connect. Point it at your relay and it picks the newest version you both
support. (You should try [moq-lite](/concept/layer/moq-lite) too, btw.)

## Install

```bash
brew install moq-dev/tap/moq-cli   # macOS / Linux
cargo install moq-cli              # any platform with Rust
docker pull moqdev/moq-cli         # or podman
```

You also need FFmpeg for encode/decode.

## Publish

A test pattern plus tone, so you don't need a media file:

```bash
ffmpeg -re -f lavfi -i testsrc=size=1280x720:rate=30 -f lavfi -i sine=frequency=440 \
    -c:v libx264 -preset ultrafast -tune zerolatency -g 60 -c:a aac \
    -f mp4 -movflags cmaf+frag_keyframe+empty_moov+default_base_moof - \
| moq --client-connect https://your-relay.example.com --broadcast bbb.hang import fmp4
```

## Subscribe

```bash
moq --client-connect https://your-relay.example.com --broadcast bbb.hang export fmp4 | ffplay -
```

If it plays, you interop. That's the whole test.

## Notes

- **`SUBSCRIBE_NAMESPACE` is required.** The subscriber discovers broadcasts by
  sending `SUBSCRIBE_NAMESPACE` and waiting for a matching announce, so your
  relay must support it. The publisher announces with `PUBLISH_NAMESPACE`.
- **Self-signed or expired cert?** Add `--client-tls-disable-verify`.
- **Subscriber sees nothing?** If your relay doesn't replay existing
  announcements, start the subscriber before the publisher.
- **Verbose logs:** prefix with `RUST_LOG=info,moq_net=debug`. It prints the
  negotiated version (e.g. `connected version=moq-transport-18`).
