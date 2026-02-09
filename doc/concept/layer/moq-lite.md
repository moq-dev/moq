---
title: MoQ Lite
description: A fraction of the calories with none of the fat.
---

# moq-lite
A subset of the [MoqTransport](/concept/standard/moq-transport) specification.
The useless/optional cruft has been removed so more time can be spent on the core functionality.

See the draft: [draft-lcurley-moq-lite](https://www.ietf.org/archive/id/draft-lcurley-moq-lite-02.html).

## Definitions
- **Broadcast** - A named and discoverable collection of **tracks** from a single publisher.
- **Track** - A series of **groups**, potentially delivered out-of-order until closed/cancelled.
- **Group** - A series of **frames** delivered in order until closed/cancelled.
- **Frame** - A chunk of bytes with an upfront size.

**NOTE:** Some things have been renamed from the IETF draft.
It's less ambiguous and closer to media terminology:
- `Namespace` -> `Broadcast`
- `Object` -> `Frame`

## Major Differences
The main goal is to reduce complexity and make the protocol easier to implement.
When a feature has limited use-cases, it's removed (for now).

- **No Request IDs**: A bidirectional stream for each request to avoid HoLB.
- **No Push**: A subscriber must explicitly subscribe to each track.
- **No FETCH**: The plan is to use HTTP for VOD instead of reinventing the wheel.
- **No Joining Fetch**: Subscriptions start at the latest group, not the latest frame.
- **No sub-groups**: SVC layers should be separate tracks.
- **No gaps**: Makes life easier for a relay.
- **No object properties**: Encode your metadata into the frame payload.
- **No pausing**: Unsubscribe if you don't want a track.
- **No binary names**: Uses UTF-8 strings instead of arrays of byte arrays.
- **No datagrams**: Maybe in the future.
