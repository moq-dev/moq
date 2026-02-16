---
title: LOC - Low Overhead Container
description: A low-overhead container format for MoQ.
---

# LOC - Low Overhead Container
We originally wanted to use [CMAF](/concept/standard/msf) but there's a lot of overhead.
Like 100 bytes per frame sort of overhead (`moof` + `mdat`), the type of overhead that kills audio-only streams.

LOC is a super simple container format that's designed to be lightweight.
It's similar to the [hang container](../layer/hang) and we'll probably merge them in the future.

[See the draft](https://www.ietf.org/archive/id/draft-ietf-moq-loc-00.html) for the latest details.
