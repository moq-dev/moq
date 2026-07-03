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

A freshly-connected WHEP / WHIP-out peer subscribes at the *current*
(in-progress) MoQ group, which begins at a keyframe, so it gets a
decodable start without waiting for the next GOP boundary. If the peer
loses keyframe packets, str0m fulfils its NACK retransmissions from the
video send buffer, which is sized to cover a large keyframe plus the rest
of the current group. MoQ has no PLI path back to the publisher, so
`KeyframeRequest` (PLI/FIR) events from the peer are logged but not
propagated upstream.

The egress paths (WHEP server, WHIP client) negotiate H.264, H.265, VP8,
VP9, AV1, and Opus. The ingest paths (WHIP server, WHEP client) currently
accept H.264, VP8, VP9, and Opus; H.265 / AV1 ingest is a follow-up.

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
- `--public-addr`: optional public UDP socket address(es) to advertise as
  ICE host candidates. Repeat the flag (or comma-separate) for dual-stack
  IPv4 + IPv6 deployments. When empty, str0m discovers peer-reflexive
  candidates via STUN binding requests, which works for most NAT
  scenarios.

### Server flags

- `--listen`: HTTP bind address (default `[::]:8088`).
- `--udp-bind`: UDP address the shared WebRTC media socket binds to
  (default `0.0.0.0:0`, i.e. an OS-picked port for dev/loopback). Every
  WHIP/WHEP session shares this one port (demuxed by ICE ufrag), so a
  deployment behind a firewall pins it (e.g. `0.0.0.0:8089`) and opens just
  that one media port. Pair it with `--public-addr` so the advertised ICE
  candidate uses the pinned port.
- `--tls-cert` / `--tls-key`: serve HTTPS instead. Most WHIP clients
  require it in practice.

### Client flags

- `--url`: remote WHIP or WHEP resource URL.

### Session teardown

The bundled WHIP/WHEP servers honor an HTTP `DELETE` to the resource URL
returned in the `Location` header (`/<broadcast>/<resource-id>`), per
RFC 9725. It ends the session promptly, releasing its broadcast
announcement and shared-media-port registration instead of waiting for the
ICE disconnect timeout. Embedders that own their own routing can call
`Server::terminate(resource_id)` to do the same.

## Codec mapping

| WebRTC codec | MoQ catalog | Egress | Ingest |
|--------------|-------------|--------|--------|
| Opus         | `AudioCodec::Opus`, 48 kHz / stereo | yes | yes |
| H.264        | `VideoCodec::H264` (avc3 inline or avc1 + `avcC`) | yes | yes (avc3) |
| H.265        | `VideoCodec::H265` (hev1 inline or hvc1 + `hvcC`) | yes | no |
| VP8          | `VideoCodec::VP8` | yes | yes |
| VP9          | `VideoCodec::VP9` | yes | yes |
| AV1          | `VideoCodec::AV1` | yes | no |

On egress, `codec::Track` reshapes each rendition into what str0m's Frame
API expects. Opus / VP8 / VP9 / AV1 and inline-parameter H.264 (avc3) /
H.265 (hev1) pass through untouched. Out-of-band-parameter H.264 (avc1) and
H.265 (hvc1) are rewritten from length-prefixed NALU to Annex-B with the
parameter sets (SPS/PPS, plus VPS for H.265) prepended to each keyframe.
This reuses `moq-mux`'s `parse_avcc_param_sets` / `parse_hvcc_param_sets`
and `annexb` helpers, the same logic as its `h264::Export` / `h265::Export`.

On ingest, H.264 is reassembled by str0m as Annex-B; `moq-mux`'s H.264
importer in `Avc3` mode publishes the inline-parameter shape directly, which
lines up with what the WebCodecs decoder in `@moq/watch` already expects. No
extra conversion needed in the gateway.
