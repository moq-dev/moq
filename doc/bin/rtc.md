---
title: moq-rtc
description: WebRTC <-> MoQ gateway (WHIP/WHEP)
---

# moq-rtc

`moq-rtc` bridges WebRTC and Media over QUIC. It speaks
[WHIP](https://datatracker.ietf.org/doc/html/rfc9725) (publish) and WHEP
(subscribe) in **either HTTP role**, so it can either accept incoming peers
or dial out to a remote WebRTC server.

## The 2x2

| Subcommand | WebRTC role | Direction | Status |
|---|---|---|---|
| `server publish` | accept WHIP publishes | RTP into MoQ | working |
| `client subscribe` | dial a remote WHEP URL | RTP into MoQ | working |
| `server subscribe` | serve WHEP subscriptions | MoQ -> RTP | working |
| `client publish` | dial a remote WHIP URL | MoQ -> RTP | working |

All four paths work. The egress paths use str0m's Frame API to packetize
MoQ frames back into RTP; the per-codec adapters live in `codec::Track`
and are the same shape regardless of HTTP role.

### Keyframe latency on the egress side

WebRTC subscribers expect a keyframe within ~2 s of joining. If the
upstream MoQ broadcast uses long GOPs, freshly-connected WHEP / WHIP-out
peers see a black screen until the next natural keyframe arrives.
`KeyframeRequest` events from the peer are logged but not propagated
upstream; PLI-to-MoQ back-pressure is a future enhancement.

AV1 / H.265 aren't in str0m 0.19's codec enum, so they're not negotiated;
this is tracked as a follow-up. Use H.264 or VP9 for now.

## CLI shape

Mirrors `moq-cli`: globals first, then HTTP role, then direction.

```bash
# server publish (WHIP server): accept publishes into MoQ
moq-rtc --relay https://relay.example.com --broadcast my-stream \
        server --listen 0.0.0.0:8088 publish

# client subscribe (WHEP client): pull from a remote WHEP source
moq-rtc --relay https://relay.example.com --broadcast cam0 \
        client --url https://camera.example.com/whep/cam0 subscribe

# server subscribe (WHEP server): serve a MoQ broadcast over WHEP
moq-rtc --relay https://relay.example.com --broadcast my-stream \
        server --listen 0.0.0.0:8088 subscribe

# client publish (WHIP client): push a MoQ broadcast to a remote WHIP endpoint
moq-rtc --relay https://relay.example.com --broadcast my-stream \
        client --url https://twitch.tv/whip publish
```

### Global flags

- `--relay`: upstream MoQ relay to publish to / subscribe from.
- `--broadcast`: MoQ broadcast name this gateway binds to.
- `--public-addr`: optional public UDP socket address to advertise as an
  ICE host candidate. When unset, str0m discovers peer-reflexive
  candidates via STUN binding requests, which works for most NAT
  scenarios. Set this only when the gateway needs an explicit external
  address.

### Server flags

- `--listen`: HTTP bind address (default `[::]:8088`).
- `--tls-cert` / `--tls-key`: serve HTTPS instead. Most WHIP clients
  require it in practice.

### Client flags

- `--url`: remote WHIP or WHEP resource URL.

## Codec mapping

| WebRTC codec | MoQ catalog |
|--------------|-------------|
| Opus         | `AudioCodec::Opus`, 48 kHz / stereo |
| H.264        | `H264 { inline: true }` (Annex-B in catalog, no `avcC`) |
| VP8          | `VideoCodec::VP8` |
| VP9          | `VideoCodec::VP9` |

H.264 input is reassembled by str0m as Annex-B; `moq-mux`'s H.264 importer
in `Avc3` mode publishes the inline-parameter shape directly, which lines
up with what the WebCodecs decoder in `@moq/watch` already expects. No
extra conversion needed in the gateway.

(Written by Claude)
