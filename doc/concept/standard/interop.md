---
title: Interoperability
description: Publish and subscribe to a moq-transport relay with moq-cli
---

# Interoperability

`moq-cli` speaks moq-transport drafts **14 through 19**, negotiated over ALPN at
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

- **We announce without being asked, and we ask.** Every namespace we can offer
  goes out as an unsolicited `PUBLISH_NAMESPACE`, and we send
  `SUBSCRIBE_NAMESPACE` for every prefix we're allowed to discover. Nothing in
  moq-transport says which one a peer expects, and the peers that never ask are
  the ones expecting to be told, so we do both by default.
- **Tell us to stop and we will.** The [MoQ Solicit
  extension](/draft/moq-solicit) is a `SETUP` option declaring what you require
  to be solicited: bit `0x1` says advertisements must be asked for and you'll
  send `SUBSCRIBE_NAMESPACE`, bit `0x2` says asking you is pointless because you
  advertise nothing. Declaring `0x1` gets you the relay behavior the IETF draft
  describes. Unknown options are ignored, so an implementation that doesn't know
  it loses nothing.
- **One namespace, one message.** We never advertise the same namespace as both
  `PUBLISH_NAMESPACE` and `NAMESPACE` on one session; your declaration picks
  which. Rejecting a `PUBLISH_NAMESPACE` is fine and keeps the session up.
- **`PUBLISH` is declined.** Content is routed per namespace and tracks are
  resolved on demand via `SUBSCRIBE`, so a single-track `PUBLISH` offer is
  answered with a request error. Announce with `PUBLISH_NAMESPACE` and serve
  the resulting `SUBSCRIBE`s instead.
- **Self-signed or expired cert?** Add `--client-tls-disable-verify`.
- **Subscriber sees nothing?** If your relay doesn't replay existing
  announcements, start the subscriber before the publisher.
- **Verbose logs:** prefix with `RUST_LOG=info,moq_net=debug`. It prints the
  negotiated version (e.g. `connected version=moq-transport-19`).
