---
title: MSF - MoQ Streaming Format
description: A catalog format for MoQ.
---

# MSF - MoQ Streaming Format

HLS/DASH playlists suck.
WebRTC SDP is even worse.
MSF is a replacement for both, utilizing MoQ live streams.

[MSF](https://www.ietf.org/archive/id/draft-ietf-moq-msf-01.html) is a catalog format for MoQ.
It's similar to the [hang catalog](../layer/hang) and we'll probably merge them in the future.

We track draft-01, which changed the catalog `version` from a number to a `"draft-XX"` string.
The older numeric form from draft-00 still decodes for backwards compatibility.

[See the draft](https://www.ietf.org/archive/id/draft-ietf-moq-msf-01.html) for the latest details.
