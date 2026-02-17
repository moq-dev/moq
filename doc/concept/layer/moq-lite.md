---
title: MoQ Lite
description: A fraction of the calories with none of the fat.
---

# moq-lite
[moq-lite](https://www.ietf.org/archive/id/draft-lcurley-moq-lite-02.html) is a subset of the [MoqTransport](/concept/standard/moq-transport) specification.
The goal is to keep the core transport layer simple and focused on practical use-cases.

There's too much fringe functionality in the MoqTransport draft that's not practical to implement.
Most of it is specific to Cisco's implementation and bizarre requirements, so it probably won't impact you.

## Compatibility
`moq-lite` is forward compatible with `moq-transport`.
That means for every moq-lite API, there's a corresponding moq-transport API.

That's good!
You're not locked into moq-lite and can use moq-transport in the future.
I can get hit by a bus and you wouldn't shed a tear.

Both [moq-lite](/rs/crate/moq-lite) and [@moq/lite](/js/@moq/lite) negotiate the `moq-lite` or `moq-transport` version as part of the QUIC/WebTransport handshake (via ALPN).
When `moq-transport` wire format is negotiated, we still enforce the moq-lite API.
For example, if there's a gap in a group (valid in moq-transport), we drop the tail of the group instead of erroring.

The following table shows the simplified compatibility matrix.
Note that there are typically 2 clients, a publisher and a subscriber.
But if a publisher needs a feature, then the subscriber needs it too, so you can lump them together.

| client        | relay         | supported | notes                                                                |
|---------------|---------------|:---------:|----------------------------------------------------------------------|
| moq-lite      | moq-lite      | ✅        |                                                                      |
| moq-lite      | moq-transport | ✅        |                                                                      |
| moq-transport | moq-lite      | ⚠️        | No moq-transport-only features.                                      |
| moq-transport | moq-transport | ⚠️        | Depends on the implementation.                                       |

Obviously I'm biased, but I wouldn't recommend using moq-transport yet.
Each new draft version (every 2 months) introduces a lot of churn and changes.
We have regular interoperability tests and it's never flawless.

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

- **No Request IDs**: A bidirectional stream for each request to avoid HoLB. (NOTE: likely to be upstreamed into moq-transport)
- **No Push**: A subscriber must explicitly subscribe to each track.
- **No FETCH**: Use HTTP for VOD instead of reinventing the wheel.
- **No Joining Fetch**: Subscriptions start at the latest group, not the latest frame.
- **No sub-groups**: SVC layers should be separate tracks.
- **No gaps**: Makes life much easier for the relay and every application.
- **No object properties**: Encode your metadata into the frame payload.
- **No pausing**: Unsubscribe if you don't want a track.
- **No binary names**: Uses UTF-8 strings instead of arrays of byte arrays.
- **No datagrams**: Maybe one day.

This may seem like a lot of missing features, but in practice you don't need them.
For example, [MSF](/concept/standard/msf) doesn't use any of these features so it's fully compatible with moq-lite.
