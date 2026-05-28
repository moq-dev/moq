---
title: moq-rtc
description: WebRTC <-> MoQ gateway (WHIP/WHEP)
---

# moq-rtc

`moq-rtc` bridges WebRTC and Media over QUIC. It speaks
[WHIP](https://datatracker.ietf.org/doc/html/rfc9725) for ingestion and WHEP
for egress, letting any conformant WebRTC publisher (OBS, browsers, mobile
SDKs) feed a MoQ relay without shipping a custom MoQ client.

## Status

- **WHIP ingest**: working for Opus audio and H.264 / VP8 / VP9 video.
- **WHEP egress**: HTTP plumbing is in place but the per-codec
  re-packetization is not implemented yet; the endpoint returns
  `501 Not Implemented`.
- **AV1 / H.265**: not in str0m 0.19; tracked as a follow-up. Use H.264 or VP9
  for now.

The gateway is built on [str0m](https://github.com/algesten/str0m) (sans-IO
WebRTC), [axum](https://github.com/tokio-rs/axum) for the HTTP layer, and
`moq-mux` for the catalog and container side.

## Run

```bash
moq-rtc \
  --listen 0.0.0.0:8088 \
  --relay https://relay.example.com \
  --ice-candidate 198.51.100.7:0
```

- `--listen`: address for the WHIP/WHEP HTTP endpoints. Use `--tls-cert` and
  `--tls-key` to serve HTTPS instead (most WHIP clients require it).
- `--relay`: upstream MoQ relay to forward ingested broadcasts to.
- `--ice-candidate`: public UDP socket(s) to advertise as ICE host
  candidates. Required behind NAT; the port component is replaced with the
  kernel-picked port at bind time.

## Endpoints

- `POST /whip/<broadcast-path>` (`Content-Type: application/sdp`): accept a
  WHIP offer, return the SDP answer. The broadcast is published to the
  upstream relay at `<broadcast-path>`.
- `POST /whep/<broadcast-path>`: WHEP egress (placeholder; see Status).

## Codec mapping

| WebRTC codec | MoQ catalog |
|--------------|-------------|
| Opus         | `AudioCodec::Opus`, 48 kHz / stereo |
| H.264        | `H264 { inline: true }` (Annex-B in catalog, no `avcC`) |
| VP8          | `VideoCodec::VP8` |
| VP9          | `VideoCodec::VP9` |

H.264 input is reassembled by `str0m` as Annex-B; `moq-mux`'s H.264 importer
in `Avc3` mode publishes the inline-parameter shape directly, which lines up
with what the WebCodecs decoder in `@moq/watch` already expects. No extra
conversion is needed in the gateway.

(Written by Claude)
