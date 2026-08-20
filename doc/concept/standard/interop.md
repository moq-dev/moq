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
| moq --connect https://your-relay.example.com --broadcast bbb.hang import fmp4
```

## Subscribe

```bash
moq --connect https://your-relay.example.com --broadcast bbb.hang export fmp4 | ffplay -
```

If it plays, you interop. That's the whole test.

## Notes

- **We announce without being asked, and we ask.** Every namespace we can offer
  goes out as an unsolicited `PUBLISH_NAMESPACE`, and we send
  `SUBSCRIBE_NAMESPACE` for every prefix we're allowed to discover. Nothing in
  moq-transport says which one a peer expects, and the peers that never ask are
  the ones expecting to be told, so we do both by default.
- **Tell us to stop and we will.** The [MoQ Solicit
  extension](/draft/moq-solicit) is a `SETUP` option set to `1` to declare that
  advertisements to you must be asked for, because you'll send
  `SUBSCRIBE_NAMESPACE` for what you want. Declaring it gets you the relay
  behavior the IETF draft describes. Unknown options are ignored, so an
  implementation that doesn't know it loses nothing.
- **We always declare it ourselves.** We ask for every prefix we're allowed to
  discover, so an unsolicited `PUBLISH_NAMESPACE` can't tell us anything we
  won't have asked for. Honor it and you'll never send us one.
- **If you implement it, we hold you to it.** Sending the option at all,
  including `0`, says you read ours. An unsolicited `PUBLISH_NAMESPACE` after
  that is a protocol violation and we close the session, because the alternative
  is a bug neither of us ever sees. Omit the option and you get the tolerant
  path instead: announce away, we'll take it. Draft-14/15 are exempt, since a
  `PUBLISH_NAMESPACE` is also how you answer a `SUBSCRIBE_NAMESPACE` there.
- **We ask everyone.** There's no way to tell us not to bother: answering a
  `SUBSCRIBE_NAMESPACE` with an empty set costs one stream, while waiting on
  your `SETUP` to find out costs a round trip on every connection.
- **One namespace, one message.** We never advertise the same namespace as both
  `PUBLISH_NAMESPACE` and `NAMESPACE` on one session; your declaration picks
  which. Rejecting a `PUBLISH_NAMESPACE` is fine and keeps the session up.
- **`PUBLISH` is declined.** Content is routed per namespace and tracks are
  resolved on demand via `SUBSCRIBE`, so a single-track `PUBLISH` offer is
  answered with a request error. Announce with `PUBLISH_NAMESPACE` and serve
  the resulting `SUBSCRIBE`s instead.
- **Self-signed or expired cert?** Add `--connect-tls-insecure`.
- **Subscriber sees nothing?** If your relay doesn't replay existing
  announcements, start the subscriber before the publisher.
- **Verbose logs:** prefix with `RUST_LOG=info,moq_net=debug`. It prints the
  negotiated version (e.g. `connected version=moq-transport-19`).
