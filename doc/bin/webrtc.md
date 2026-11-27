---
title: WebRTC
description: WHIP and WHEP gateway between WebRTC and MoQ
---

# WebRTC

`moq import webrtc` and `moq export webrtc` bridge WebRTC using
[WHIP](https://datatracker.ietf.org/doc/html/rfc9725) (publish) and WHEP
(play) in either HTTP role, so existing WebRTC clients and services plug into
MoQ broadcasts.

| Command | Role | Media flow |
| --- | --- | --- |
| `import webrtc --listen` | WHIP server | Browsers publish to you |
| `import webrtc --connect <url>` | WHEP client | Pull from a remote WebRTC source (a camera, an SFU) |
| `export webrtc --listen` | WHEP server | Serve a broadcast to WebRTC players |
| `export webrtc --connect <url>` | WHIP client | Push a broadcast to a remote WHIP endpoint |

```bash
moq --connect https://relay.example.com/anon --broadcast cam.hang import webrtc --listen '[::]:8080'
moq --connect https://relay.example.com/anon --broadcast cam.hang export webrtc --listen '[::]:8080'
```

Peers reach the broadcast at `http://host:8080/<broadcast>`. Opus, H.264,
H.265, VP8, VP9, and AV1 are negotiated in both directions. A WHEP peer
receives the largest enabled video rendition (then highest bitrate) in its negotiated codec, and
the highest-bitrate enabled Opus rendition (then sample rate, then channels). It
follows the catalog: a peer whose rendition is disabled moves to the next best at
that rendition's first keyframe, a better one that re-enables wins the peer back,
and with none enabled the peer receives nothing until one is. A WHEP offer gets one
audio and one video line at most: the answer rejects the rest, which would only repeat
the same media. `--cors-origin`
opens the endpoint to browsers on other origins. Every session shares one UDP
media port, `--udp-bind` (OS-picked by default), bound at startup so a taken port
fails before the listener reports ready. Behind NAT, `--public-addr` advertises
the public address as written, so `--public-addr 203.0.113.1:5000` reaches a port
the NAT forwards to `--udp-bind`; port 0 advertises the bound port. The listener is plain HTTP;
put a TLS-terminating proxy in front for WHIP clients that require HTTPS. A fresh WHEP peer joins at the current group, so it
starts at a keyframe without waiting for the next one. `DELETE` on the
resource URL tears a session down per the RFC. Each WHIP session publishes
under a fresh [identity](/concept/moq-lite#publisher-identities), so a publisher that
reconnects while its stale session lingers replaces it at once.

An export stays on the publisher it started with. A replacement publisher never
reaches its session, so timestamps and codec state never reset mid-session.
Once the original publisher leaves, the session ends with an error, and the
player or `export webrtc --connect` must reconnect to reach the replacement. A
publisher that ends cleanly, such as an RTMP `deleteStream`, ends the session
cleanly once its last frames are sent: the peer connection closes with an RTCP BYE
and a DTLS close\_notify, which a browser reports as its `RTCDtlsTransport` state
turning `closed`.
`--connect` runs until its session ends.

Built on [str0m](https://github.com/algesten/str0m), with DTLS and SRTP on [rind](https://github.com/kixel-ai/rind); the library is
[`moq-webrtc`](https://github.com/moq-dev/moq/tree/main/rs/moq-webrtc).
