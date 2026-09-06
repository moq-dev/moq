---
title: WebRTC
description: WHIP and WHEP gateway between WebRTC and MoQ
---

# WebRTC

`moq import rtc` and `moq export rtc` bridge WebRTC using
[WHIP](https://datatracker.ietf.org/doc/html/rfc9725) (publish) and WHEP
(play) in either HTTP role, so existing WebRTC clients and services plug into
MoQ broadcasts.

| Command | Role | Media flow |
| --- | --- | --- |
| `import rtc --listen` | WHIP server | Browsers publish to you |
| `import rtc --connect <url>` | WHEP client | Pull from a remote WebRTC source (a camera, an SFU) |
| `export rtc --listen` | WHEP server | Serve a broadcast to WebRTC players |
| `export rtc --connect <url>` | WHIP client | Push a broadcast to a remote WHIP endpoint |

```bash
moq --client-connect https://relay.example.com/anon --broadcast cam.hang import rtc --listen '[::]:8080'
moq --client-connect https://relay.example.com/anon --broadcast cam.hang export rtc --listen '[::]:8080'
```

Peers reach the broadcast at `http://host:8080/<broadcast>`. Opus, H.264,
H.265, VP8, VP9, and AV1 are negotiated in both directions. `--cors-origin`
opens the endpoint to browsers on other origins, and `--udp-bind` plus
`--public-addr` pin one media port for firewalls. The listener is plain HTTP;
put a TLS-terminating proxy in front for WHIP clients that require HTTPS. A fresh WHEP peer joins at the current group, so it
starts at a keyframe without waiting for the next one. `DELETE` on the
resource URL tears a session down per the RFC.

Built on [str0m](https://crates.io/crates/str0m); the library is
[`moq-rtc`](https://docs.rs/moq-rtc).
