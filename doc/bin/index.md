---
title: Applications
description: Ready-to-use tools built on MoQ
---

# Applications

Use these tools to operate MoQ or bridge it to an existing media workflow.

## Core tools

| Tool | Use it to |
| --- | --- |
| [moq-relay](/bin/relay/) | Route, cache, and fan out broadcasts. Relays can also form multi-region clusters. |
| [moq-cli](/bin/cli) | Publish, subscribe, play, transcode, or convert media from the command line. |
| [Web demo](/bin/web) | Publish and watch from a browser with the project web components. |

## Protocol gateways

| Gateway | Direction |
| --- | --- |
| [moq-hls](/bin/hls) | Import HLS into MoQ or serve a MoQ broadcast as HLS. |
| [moq-rtmp](/bin/rtmp) | Accept RTMP and enhanced RTMP publishers and forward them into MoQ. |
| [moq-rtc](/bin/rtc) | Bridge WHIP/WHEP and MoQ in either client or server roles. |

These pages document gateway endpoints exposed by `moq-cli` and the libraries
that implement them.

## Media integrations

| Integration | Use it to |
| --- | --- |
| [OBS plugin](/bin/obs) | Publish a scene to MoQ or use a MoQ broadcast as an OBS source. |
| [GStreamer plugin](/bin/gstreamer) | Add MoQ source and sink elements to a GStreamer pipeline. |

To embed MoQ directly in another application, choose a [library](/lib/) instead.
