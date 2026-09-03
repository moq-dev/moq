---
title: Layers
description: How the MoQ protocol stack fits together
---

# Layers

MoQ separates transport, live data delivery, media packaging, and application
behavior. A relay only needs the delivery layer, while media clients use the
whole stack.

| Layer | Role |
| --- | --- |
| Application | Defines product behavior and any custom tracks. |
| [hang](/concept/layer/hang) | Describes media tracks, codecs, and frame timestamps. |
| [moq-lite](/concept/layer/moq-lite) | Publishes and subscribes to generic broadcasts, tracks, groups, and frames. |
| Transport | Carries independent streams over QUIC, WebTransport, WebSocket, or iroh. |

## Transport

[QUIC](/concept/layer/quic) provides encrypted connections, independent streams,
congestion control, and connection migration. Avoiding connection-wide
head-of-line blocking is the main reason MoQ uses QUIC.

Browsers access QUIC through [WebTransport](/concept/layer/web-transport).
Clients can race it against the [WebSocket fallback](/concept/layer/web-socket)
when UDP or WebTransport is unavailable. Native peers can use QUIC directly or
connect through [iroh](/concept/layer/iroh) on a local network.

## Live delivery

[moq-lite](/concept/layer/moq-lite) is the generic pub/sub layer. It is a
forward-compatible subset of [MoqTransport](/concept/standard/moq-transport)
and does not assign media meaning to payloads.

A session can publish and subscribe at the same time:

- A **broadcast** contains one or more named **tracks**.
- A **track** is an ordered sequence of independently delivered **groups**.
- A **group** contains reliably ordered **frames** and can be canceled without
  blocking other groups.

Relays use these boundaries for caching, fan-out, and prioritization without
parsing codecs or application data.

## Media

[hang](/concept/layer/hang) defines the media information endpoints need to
share: a track catalog, codec configuration, timestamps, and frame containers.
The relay remains unaware of those details.

## Application data

Applications can add tracks for control messages, metadata, telemetry, or other
live data. Unknown tracks do not change relay behavior or interfere with media
tracks.

Start with [moq-lite](/concept/layer/moq-lite) and
[hang](/concept/layer/hang), then use the [standards overview](/concept/standard/)
to understand the wider protocol ecosystem.
