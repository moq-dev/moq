---
title: MoQ vs WebRTC
description: How MoQ compares to conferencing protocols
---

# MoQ vs WebRTC
This page compares MoQ with **WebRTC**, the dominant protocol for conferencing.

## Requirements
Boring stuff first.
Conferencing protocols need to:

- Support any number of participants (publish and subscribe)
- Support <100ms latency
- Support a wide range of devices
- Support a wide range of networks

Some optional features:
- Support browsers (aka WebRTC)
- End-to-end encryption.
- Peer-to-peer connections.

## Existing Protocols
- **WebRTC** ([Web Real-Time Communication](https://en.wikipedia.org/wiki/WebRTC)) - The dominant protocol for conferencing.
- **RTP** ([Real-Time Transport Protocol](https://en.wikipedia.org/wiki/Real-time_Transport_Protocol)) - The core protocol within WebRTC.

While this might make WebRTC seem super dominant, the reality is a little bit more nuanced.

Almost every conferencing service tries to force their native app on you.
Zoom, Teams, Discord, etc.
WebRTC is mandatory on the browser, but it's *not* mandatory for native apps.
A service like Discord uses a custom RTP stack between native apps and only uses WebRTC for browser compatibility.

The only exception is Google Meet.
Google maintains and controls `libwebrtc`, the core WebRTC implementation in browsers.
If Google wants a feature, then they add it to WebRTC, while every other service has to find a workaround.
