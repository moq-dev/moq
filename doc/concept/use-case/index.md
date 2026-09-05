---
title: Use Cases
description: Where MoQ fits, compared with what it replaces
---

# Use Cases

One protocol covers contribution, distribution, conferencing, and data. That
is the point: sharing an implementation across all four is where the economies
of scale come from.

| Use case | Replaces | Why MoQ fits |
| --- | --- | --- |
| [Distribution](/concept/use-case/distribution) | HLS, DASH | Sub-second latency at CDN scale, with the viewer choosing its buffer. |
| [Contribution](/concept/use-case/contribution) | RTMP, SRT | Pull-based delivery, optional on-demand encoding, and simple redundant ingest. |
| [Conferencing](/concept/use-case/conferencing) | WebRTC | Bidirectional on one session, browser control of the pipeline, no SDP or TURN. |
| [AI](/concept/use-case/ai) | WebRTC, WebSockets | Adjustable reliability, on-demand inference, media and prompts on one connection. |
