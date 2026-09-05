---
title: MoQ vs WebRTC
description: How MoQ compares with WebRTC for real-time conferencing
---

# MoQ vs WebRTC

WebRTC is the integrated real-time communications stack browsers ship, so it
is what conferencing uses in a tab. It is less dominant than it looks: Zoom, Teams,
and Discord run custom RTP stacks in their native apps and keep WebRTC for
browser compatibility, and the one vendor that controls `libwebrtc` gets its
features first.

## What MoQ offers

- **Publish and subscribe on one session.** Each participant is a broadcast; peers discover each other through announcements on a path prefix. No SDP offer/answer, no renegotiation when someone joins.
- **Latency you choose.** WebRTC decides when to drop. A MoQ subscriber sets its own budget per track, so audio can be near-lossless at 500 ms while video stays at the live edge.
- **Scale from the same relay.** A relay that serves a two-person call also serves a ten-thousand-viewer webinar. There is no SFU/CDN split.
- **Full pipeline control in the browser.** WebCodecs frames and `AudioData` samples are yours: render to a texture, run a model, apply effects.
- **No STUN/TURN.** Clients dial the relay over WebTransport like any HTTPS server. Native peers on a LAN can go [peer-to-peer over iroh](/concept/transport#iroh-peer-to-peer-experimental).

## Interop

WebRTC clients still fit. The [WebRTC gateway](/bin/rtc) accepts WHIP
publishers and serves WHEP players, bridging them into the same broadcasts the
MoQ clients use.
