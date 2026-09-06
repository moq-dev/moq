---
title: moq-lite
description: The generic pub/sub layer, a simple forward-compatible subset of moq-transport
---

# moq-lite

moq-lite is the pub/sub protocol this project speaks. It is a deliberately
small subset of the IETF [moq-transport](/concept/standard) draft, so it works
against any moq-transport relay (including
[Cloudflare](https://moq.dev/blog/first-cdn)) while staying simple enough to
implement in an afternoon. The wire spec is
[draft-lcurley-moq-lite](/draft/moq-lite).

## Terminology

| moq-lite | Meaning | moq-transport name |
| --- | --- | --- |
| **Session** | One connection, publishing and subscribing at once. | Session |
| **Origin** | The set of broadcasts visible to a session, scoped by the URL path. | (none) |
| **Broadcast** | A named, discoverable collection of tracks from one publisher. | Namespace |
| **Track** | A live sequence of groups, delivered out of order until closed. | Track |
| **Group** | A sequence of frames delivered reliably and in order, on its own QUIC stream. | Group |
| **Frame** | A sized chunk of bytes. | Object |
| **Datagram** | One unreliable frame sent as a QUIC datagram instead of a group. | Datagram |

## Session setup

The ALPN picks the protocol family, and a single `SETUP` message from each side
negotiates the version and capabilities. Neither side waits for the other. The
Rust and TypeScript stacks currently speak moq-lite 01 through 05 (06 is in
progress) and moq-transport drafts 14 through 20, and a client offers all of
them by default.

## Discovery

A session can ask for announcements matching a path prefix. The peer replies
with the broadcasts currently live, then streams changes as they come and go.
That is how a conference room learns who joined, how a player learns a stream
came online without polling, and how [relay clusters](/bin/relay/cluster)
discover each other.

## Subscriptions

A subscriber names a broadcast and track. New subscriptions start at the
latest group, so every group must begin at a point a fresh subscriber can
decode from (a keyframe, a full JSON snapshot). Groups can be fetched by
sequence number too, which is how the [HLS gateway](/bin/hls) and the relay's
[HTTP fetch](/bin/relay/http) serve history.

Each subscription carries the knobs that decide behavior under congestion:

| Knob | Effect |
| --- | --- |
| **Priority** (0..255) | Higher-priority tracks get bandwidth first. Audio above video, base layer above enhancement. |
| **Order** | Which group to send first when several are pending. Newest first for live, oldest first for catch-up. |
| **Max latency** | How long a stale group is waited for before it is skipped. Zero means "live edge only". |

The publisher declares a retention window per track, which bounds how far back
a fetch or late subscriber can reach. Media tracks default to 30 seconds so a
segmented egress can still find its segments.

Put together, a conference might use:

| Track | Priority | Order | Max latency |
| --- | --- | --- | --- |
| audio | 100 | ascending | 500 ms |
| video | 50 | descending | 2 s |

Under light congestion video drops the tail of a group; under heavy congestion
video stops and audio lags by at most 500 ms. No protocol change, just knobs.

## Datagrams

Since moq-lite 05, a publisher can send a tiny single-frame group as a QUIC
datagram: unreliable, unordered, under about 1200 bytes, and never
retransmitted. It suits real-time audio and sensor data. There is no stream
fallback, so a datagram that doesn't fit isn't delivered that way.

## What moq-lite leaves out

Compared with moq-transport: no request IDs (a stream per request instead), no
push (subscribers always ask), single-group fetches only, no sub-groups (use a
track per SVC layer), no gaps in object numbering, no per-object metadata
(encode it in the payload), no pausing (unsubscribe instead), and UTF-8 names
instead of byte arrays. When a peer negotiates moq-transport the implementation
still enforces this simpler model, faking or refusing the rest.

| Client | Relay | Works |
| --- | --- | --- |
| moq-lite | moq-lite | yes |
| moq-lite | moq-transport | yes |
| moq-transport | moq-lite | without moq-transport-only features |
| moq-transport | moq-transport | depends on the implementations |
