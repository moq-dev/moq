---
title: MoqTransport
description: The generic pub/sub protocol that powers everything.
---

# MoqTransport
The generic pub/sub protocol that powers everything.
It doesn't know anything about media - it just moves bytes around really efficiently.
CDNs implement this layer, and it's designed to scale to millions of subscribers.

This guide assumes that you are familiar with [moq-lite](/concept/layer/moq-lite).
Many of the concepts are the same and it's easier to point out the differences rather than start from scratch.

- [Latest Draft](https://www.ietf.org/archive/id/draft-ietf-moq-transport-16.html)

## Model
### Namespace
A named collection of related **tracks**, called a `Broadcast` in moq-lite.
Each namespace is identified by an array of byte arrays, but most people use an array of strings.

Everything is scoped, so if you `SUBSCRIBE_NAMESPACE` to `["sports", "nba"]` you'll also get `["sports", "nba", "highlights"]`.
MoqTransport also has the `forward=1` flag to automatically subscribe to any `PUBLISH`'d tracks in the namespace.

Unlike moq-lite, a namespace *could* be shared by multiple publishers.
However, I don't think CDNs will ever implement this feature due to conflicts/complexity/bugs.

### Track
A named collection of **groups**, potentially served out-of-order.
Each track is identified by a byte array, but most people use a string.

Each subscription is scoped to a single track.
Unlike moq-lite, a track can have multiple publishers.
Again, I don't think a CDN will ever implement this due to conflicts/complexity/bugs.

### Group
A collection of **sub-groups**, potentially served out-of-order.
Each group is identified by a monotonically increasing group ID, with gaps allowed.

### Sub-Group
A collection of **frames**, served in order over a single QUIC stream.
Each sub-group is identified by a sub-group ID, where 0 is considered the base layer.

Sub-groups are intended for layered encodings (SVC), such that an enhancement layer can be dropped without affecting a base layer.
In moq-lite, you would make a separate track for each layer so you can select/prioritize them independently.

### Object
The smallest unit in MoQ: a sized chunk of data.
Each object is identified by a monotonically increasing object ID.

Unlike moq-lite, there may be both explicit and implicit gaps in the object ID.
There are also "Object Properties" that can be used to store K/V metadata pairs.

## Request
MoqTransport has a bunch of different request types, each with a different purpose.
They are identified by a request ID and have corresponding _OK and _ERROR responses.

Note that `moq-lite` contains the equivalent of: `SUBSCRIBE` and `SUBSCRIBE_NAMESPACE`.
The other requests are unique to MoqTransport.

### SUBSCRIBE
Allows access to future objects.
When a new object is available, each active subscriber will receive a copy of it over the corresponding sub-group QUIC stream.

Unlike moq-lite, a subscription starts at the latest object within a group, not the first object.
This is quite annoying because you have to issue a `JOINING FETCH` request to get the earlier objects within the group (ex. the I-frame).

Unlike moq-lite, there is a `forward=0` flag to pause a subscription.

### PUBLISH
A publisher can start pushing subscription data rather than waiting for a `SUBSCRIBE`.

This can avoid a round-trip when the peer definitely wants to receive the track.
It can also be used to discover track names, as that information is not available in `PUBLISH_NAMESPACE`

### SUBSCRIBE_NAMESPACE
Announces that a namespace is newly available or unavailable.
There's no information about the tracks within the namespace.

Unlike moq-lite, there is a `forward=1` flag to automatically receive any `PUBLISH`'d tracks in the namespace.

### PUBLISH_NAMESPACE
Announces the availability of a namespace.
There's no information about the tracks within the namespace.

Unlike moq-lite, this can be sent without a corresponding `SUBSCRIBE_NAMESPACE`.

### FETCH
Allows access to past objects.
All objects that match the provided range will be delivered in Group ID, Object ID order over a single QUIC stream.

FETCH appears nice on paper, but there are numerous issues due to the HoLB blocking and implicit gaps.
I would strongly encourage fetching objects via HTTP or some other mechanism for now.

### JOINING FETCH
A special variant of FETCH that is relative to an existing subscription.
This is used to get the prior objects within a group (ex. the I-frame).
