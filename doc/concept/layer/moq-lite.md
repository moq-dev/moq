---
title: MoQ Lite
description: A fraction of the calories with none of the fat.
---

# moq-lite
A subset of the [MoqTransport](/concept/standard/moq-transport) specification.
The useless/optional cruft has been removed so more time can be spent on the core functionality.

See the draft: [draft-lcurley-moq-lite](https://www.ietf.org/archive/id/draft-lcurley-moq-lite-02.html).

## Definitions
- **Broadcast** - A named and discoverable collection of tracks from a single publisher.
- **Track** - A series of groups, potentially delivered out-of-order until closed/cancelled.
- **Group** - A series of frames delivered in order until closed/cancelled.
- **Frame** - A chunk of bytes with an upfront size.

**NOTE:** Some things have been renamed from the IETF draft, trying to keep them closer to media terminology:
- `Namespace` -> `Broadcast`
- `Object` -> `Frame`

## Major Differences
- **No Request IDs**: A bidirectional stream for each request. (note: draft-17 will copy this feature)
- **No Push**: A subscriber must explicitly request each track.
- **No FETCH**: Use HTTP for VOD.
- **No Joining Fetch**: Subscriptions start at the latest group (aka I-frame), not the latest object (aka P-frame).
- **No subgroups**: Make a new track for SVC layers.
- **No gaps**: Object IDs increase by 1 for each frame.
- **No object properties**: Put the metadata in the frame payload.
- **No paused subscriptions**: Just subscribe when you want the track.
- **No datagrams**: Maybe in the future.
- **No binary names**: Uses UTF-8 strings instead of arrays of byte arrays.
