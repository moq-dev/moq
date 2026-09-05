---
title: RTMP
description: RTMP and enhanced RTMP ingest and playback
---

# RTMP

`moq import rtmp` accepts pushes from OBS, FFmpeg, and hardware encoders;
`moq export rtmp` serves plays to VLC, ffplay, and mpv, or pushes to a remote
RTMP server such as Twitch. Both legacy RTMP (H.264 + AAC) and enhanced RTMP
(HEVC, AV1, VP9, Opus, AC-3, multitrack) work in each direction.

```bash
# Accept an OBS push and publish it to a relay.
# In OBS: server rtmp://host:1935/live, any stream key.
moq --client-connect https://relay.example.com/anon --broadcast live.hang import rtmp --listen '[::]:1935'

# Serve the broadcast to RTMP players
moq --client-connect https://relay.example.com/anon --broadcast live.hang export rtmp --listen '[::]:1935'
ffplay rtmp://localhost:1935/live

# Restream to Twitch
moq --client-connect https://relay.example.com/anon --broadcast live.hang export rtmp --connect 'rtmp://live.twitch.tv/app/<key>'
```

A listener bridges exactly one `--broadcast` and ignores the RTMP app and
stream key, so multi-tenant routing by key belongs in your own program using
the [`moq-rtmp`](https://docs.rs/moq-rtmp) library, which hands you each
publish or play request to accept, map to a path, or reject. The CLI listener
is unauthenticated; firewall it.

Implemented in pure Rust (no librtmp). The CLI speaks plaintext `rtmp://`
only; the library adds RTMPS when the embedder supplies a TLS config. FLAC and
MP3 enhanced-audio payloads are dropped because hang has no catalog codec for
them.
