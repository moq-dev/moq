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
moq --connect https://relay.example.com/anon --broadcast live.hang import rtmp --listen '[::]:1935'

# Serve the broadcast to RTMP players
moq --connect https://relay.example.com/anon --broadcast live.hang export rtmp --listen '[::]:1935'
ffplay rtmp://localhost:1935/live

# Restream to Twitch
moq --connect https://relay.example.com/anon --broadcast live.hang export rtmp --connect 'rtmp://live.twitch.tv/app/<key>'
```

A listener bridges exactly one `--broadcast` and ignores the RTMP app and
stream key, so multi-tenant routing by key belongs in your own program using
the [`moq-rtmp`](https://docs.rs/moq-rtmp) library, which hands you each
publish or play request to accept, map to a path, or reject. The CLI listener
is unauthenticated; firewall it.

Each push or pull is its own broadcast, under a fresh
[epoch](/concept/moq-lite#publisher-epochs): an encoder that reconnects while
its stale connection is still open replaces it at once. Subscriptions to the
stale push end with `Unroutable` instead of stalling, and a viewer's next
subscribe reaches the new push. Import publishes the encoder's own timestamps
and anchors the catalog clock on the first frame, so it names the wall time the
push arrived. A group starting before the previous group's start, such as an
encoder restarting its timestamps mid-push, ends that push with an error.

A player that advertises enhanced-RTMP multitrack receives every rendition.
Any other player receives one video rendition, the largest picture (then
highest bitrate) in a codec it advertised, and one audio rendition, the highest
bitrate (then sample rate, then channels) in a codec it advertised. A push
carries the best of each.

Implemented in pure Rust (no librtmp). The CLI speaks plaintext `rtmp://`
only. The library adds RTMPS on the same port when the embedder supplies a TLS
config, and by default still accepts plaintext `rtmp://` there, so stream keys
can arrive unencrypted. The embedder can set `plaintext` to `false` to serve
`rtmps://` only: a plaintext client is refused at its first byte and the refusal
is logged with the peer address. Refusing plaintext without a TLS config fails
at startup. FLAC and MP3 enhanced-audio payloads are dropped because hang has no
catalog codec for them.

Incoming chunk streams are reassembled independently, including interleaved
control, audio, and video messages. Each connection retains at most 256 chunk
stream IDs, including completed streams' header history needed by compressed
headers. Reusing an ID does not consume another slot; the connection releases
all slots on teardown. A 257th distinct ID is refused, even if its numeric ID is
small. The limit counts streams rather than constraining their numeric IDs.

Incomplete messages reserve their declared payload lengths against a 64 MiB
connection budget before allocation. Completion or an RTMP Abort releases that reservation;
malformed input or exhaustion closes the connection and releases its parser
state. The 24-bit RTMP message length still allows a payload up to 16 MiB minus
one byte. Four maximal payloads fit, with exactly four bytes remaining.

These internal limits leave room for the gateway's five chunk-stream roles
and [FFmpeg's fixed control/audio/video channels](https://github.com/FFmpeg/FFmpeg/blob/master/libavformat/rtmppkt.h)
as well as [OBS's control and media channels](https://github.com/obsproject/obs-studio/blob/master/plugins/obs-outputs/librtmp/rtmp.c).
They bound retained state independently of chunk size and retain support for
large keyframes. Undecoded input has a separate 64 MiB buffer limit.
Connections exceeding these limits are unsupported.

Run `just rs bench-rtmp` to compare decoding one active stream while 1, 16, 64,
or 256 streams retain state, across 128-byte, 4 KiB, and 64 KiB messages with
128-byte and 4 KiB chunks. The benchmark smoke runs nightly.
