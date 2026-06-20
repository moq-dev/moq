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

We track draft-01, which changed the catalog `version` from a number to a `"draft-XX"` string and
moved init data out of the track into a root `initDataList` referenced by `initRef`.
Our implementation hides this on the wire: the catalog API is a version-agnostic snapshot, draft-00
catalogs still decode, and init data is always presented inline regardless of how it was carried.

[See the draft](https://www.ietf.org/archive/id/draft-ietf-moq-msf-01.html) for the latest details.
