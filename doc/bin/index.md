---
title: Applications
description: Ready-to-run tools built on MoQ
---

# Applications

Everything here is a binary or plugin you can run without writing code. See
[Install](/setup/install) for packages.

## Core

| Tool | Use it to |
| --- | --- |
| [moq-relay](/bin/relay/) | Route, cache, and fan out broadcasts. Authenticate with JWTs, cluster across regions, publish stats. |
| [moq-cli](/bin/cli) | Move media in and out of MoQ: pipe FFmpeg in, play natively, capture a camera or screen, transcode, mint tokens. |

## Gateways

All four ship inside `moq-cli` as `moq import <gateway>` and `moq export <gateway>`.

| Gateway | Direction |
| --- | --- |
| [RTMP](/bin/rtmp) | Accept RTMP/E-RTMP pushes from OBS or FFmpeg; serve RTMP plays; restream to Twitch-style endpoints. |
| [SRT](/bin/srt) | Accept SRT contribution; serve SRT to players; pull from or push to a remote SRT endpoint. |
| [WebRTC](/bin/rtc) | WHIP ingest and WHEP playback in either HTTP role. |
| [HLS](/bin/hls) | Serve any broadcast as HLS from the relay cache; import a remote HLS playlist. |

## Plugins

| Plugin | Use it to |
| --- | --- |
| [OBS Studio](/bin/obs) | Stream from OBS to a relay, or bring a broadcast into a scene as a source. |
| [GStreamer](/bin/gstreamer) | `moqsink` and `moqsrc` elements for any pipeline. |

## Demos

The [web demo and MoQ Boy](/bin/demo) are runnable examples of browser
playback, publishing, and bidirectional input.

To embed MoQ in your own program, use a [library](/lib/) instead.
