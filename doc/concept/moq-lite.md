---
title: moq-lite
description: The generic pub/sub layer, a simple forward-compatible subset of moq-transport
---

# moq-lite

moq-lite is the pub/sub protocol this project speaks. It is a deliberately
small subset of the IETF [moq-transport](/concept/standard) draft, so it works
against any moq-transport relay (including
[Cloudflare](https://moq.dev/blog/first-cdn)) while staying simple enough to
implement in an afternoon. The wire spec is
[draft-lcurley-moq-lite](/draft/moq-lite).

## Terminology

| moq-lite | Meaning | moq-transport name |
| --- | --- | --- |
| **Session** | One connection, publishing and subscribing at once. | Session |
| **Origin** | The set of broadcasts visible to a session, scoped by the URL path. | (none) |
| **Broadcast** | A named, discoverable collection of tracks from one publisher. | Namespace |
| **Track** | A live sequence of groups, delivered out of order until closed. | Track |
| **Group** | A sequence of frames delivered reliably and in order, on its own QUIC stream. | Group |
| **Frame** | A sized chunk of bytes. | Object |
| **Datagram** | One unreliable frame sent as a QUIC datagram instead of a group. | Datagram |

## Session setup

The ALPN picks the protocol family, and a single `SETUP` message from each side
negotiates the version and capabilities. Neither side waits for the other. The
Rust and TypeScript stacks currently speak moq-lite 01 through 05 (06 is in
progress) and moq-transport drafts 14 through 20, and a client offers all of
them by default.

## Discovery

A session can ask for announcements matching a path prefix. The peer replies
with what it can serve today, then streams changes as they come and go. That is
how a conference room learns who joined, how a player learns a stream came
online without polling, and how [relay clusters](/bin/relay/cluster) discover
each other.

An announcement is a **route**: a claim that broadcasts under a prefix can be
served. By convention a publisher announces each broadcast's exact path, so
subscribers enumerate broadcasts by listing routes, but a route can also cover
a whole subtree, letting a server announce one prefix and serve whatever is
requested beneath it. Each route carries the chain of relay identities it
passed through, which is how forwarding loops are caught, and a cost, which is
how a subscriber picks the cheapest of several routes to the same broadcast.

## Path patterns

Rust's `moq_net::path::Pattern` and TypeScript's `Path.Pattern` from `@moq/net`
describe sets of literal paths. These APIs provide matching and scope algebra;
using one does not enable wildcard announcements or change token permissions.

Patterns match the whole path. `room` matches only `room`, while `room/**`
matches `room` and every descendant. Segments are separated by `/`:

| Segment | Matches |
| --- | --- |
| `room` | That literal segment. |
| `*` | Exactly one nonempty segment. |
| `camera-*` | One segment starting with `camera-`. |
| `pre*suf` | One segment with that prefix and suffix, without overlapping them. |
| `**` | Zero or more segments. |

A pattern may contain at most 32 segments and one `**`. Leading, trailing,
or repeated separators are invalid pattern syntax. The empty pattern matches
the empty path. There is no escape syntax for a literal `*`.
Construction normalizes adjacent `*` and `**`: `*/**` prints as `**/*`,
so equivalent wildcard placements have the same identity.

```typescript
import { Path } from "@moq/net";

const scope = Path.Pattern.parse("room/**");
scope.matches("room/alice"); // true
scope.contains(Path.Pattern.parse("room/camera-*")); // true
scope.overlaps(Path.Pattern.parse("*/alice")); // true
scope.rebase("room").toJSON(); // ["**"]
Path.Pattern.parse("camera-*").rooted("room").text; // "room/camera-*"
```

`contains` asks whether every path matched by the other pattern is allowed by
this one. `overlaps` asks whether they share any matching path. `rebase` returns
the matching paths relative to a literal root; `rooted` places a pattern beneath
that root and rejects results exceeding the segment limit. Rebasing can require
several patterns: `**/a` rebased at `a` yields both `""` (the root itself) and
`**/a` (deeper paths ending in `a`).

`Patterns` is a union that removes members contained by another member and
orders the remaining members canonically. Its containment check requires one
member to cover the entire requested pattern; it does not combine several
members to prove joint coverage. Equality compares those reduced members.
Use `specificity` to rank structural constraints when selecting rules, and
`contains` to check whether a rule stays within a scope.

## Subscriptions

A subscriber names a broadcast and track. Delivery starts at the oldest group
it can still use, which at the default budget is the latest one, so every group
must begin at a point a fresh subscriber can decode from (a keyframe, a full
JSON snapshot). Groups can be fetched by sequence number too, optionally
bounded to a range of frames, which is how the [HLS gateway](/bin/hls) and the
relay's [HTTP fetch](/bin/relay/http) serve history.

Each subscription carries the knobs that decide behavior under congestion:

| Knob | Effect |
| --- | --- |
| **Priority** (0..255) | Higher-priority tracks get bandwidth first. Audio above video, base layer above enhancement. |
| **Order** | Which group to send first when several are pending. Newest first for live, oldest first for catch-up. |
| **Max age** | How old a non-latest group may get before it is skipped. Zero means "live edge only", and raising it is also what asks for history. |

Max age is measured on the media timeline, not the wall clock, so a backlog
delivered as a burst is still old while a congestion stall never expires
anything on its own. Both ends apply it: the publisher skips a group rather
than sending it, and the subscriber skips it again as it reads, since the
publisher only ever sees the most tolerant budget across its subscribers.

The publisher declares a retention window per track, which bounds how far back
a fetch or late subscriber can reach. Media tracks default to 30 seconds so a
segmented egress can still find its segments.

Put together, a conference might use:

| Track | Priority | Order | Max age |
| --- | --- | --- | --- |
| audio | 100 | ascending | 500 ms |
| video | 50 | descending | 2 s |

Under light congestion video drops the tail of a group; under heavy congestion
video stops and audio lags by at most 500 ms. No protocol change, just knobs.

## Datagrams

Since moq-lite 05, a publisher can send a tiny single-frame group as a QUIC
datagram: unreliable, unordered, under about 1200 bytes, and never
retransmitted. It suits real-time audio and sensor data. There is no stream
fallback, so a datagram that doesn't fit isn't delivered that way.

## What moq-lite leaves out

Compared with moq-transport: no request IDs (a stream per request instead), no
push (subscribers always ask), fetches within a single group only, no
sub-groups (use a track per SVC layer), no gaps in object numbering, no per-object metadata
(encode it in the payload), no pausing (unsubscribe instead), and UTF-8 names
instead of byte arrays. When a peer negotiates moq-transport the implementation
still enforces this simpler model, faking or refusing the rest.

| Client | Relay | Works |
| --- | --- | --- |
| moq-lite | moq-lite | yes |
| moq-lite | moq-transport | yes |
| moq-transport | moq-lite | without moq-transport-only features |
| moq-transport | moq-transport | depends on the implementations |
