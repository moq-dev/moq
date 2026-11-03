---
title: Sockets
description: Dial and accept point-to-point connections over moq-lite, with tracks as streams
---

# Sockets

A socket connects one dialer to one listener through relays, the way a TCP
connection does, provided every hop runs moq-lite 07, which is opt-in (see
[Versions](#versions)). A listener at a path accepts the sockets that dialers open to
it, and inside a socket either side opens and accepts tracks the way QUIC opens
and accepts streams. Use it for per-client state that only the two ends need,
such as a viewer's commands and the replies to them, while whatever every
client needs stays in an ordinary broadcast.

A socket is a convention over moq-lite's announcements, broadcasts, and track
listings, not a wire extension, so relays carry it unchanged. This page is the
convention, enough to implement it in any language.
[`moq_net::socket`](https://docs.rs/moq-net/latest/moq_net/socket/) implements
it in Rust.

## Paths

A listener at `L` keeps `L` itself for what it shares with everyone. Each
socket is two broadcasts beneath it:

| Path | Published by | Holds |
| --- | --- | --- |
| `L/.dial/<id>` | The dialer | The dialer's tracks |
| `L/.accept/<id>` | The listener | The listener's tracks |

The id is one path segment that the dialer chooses, such as a random id or an
endpoint ID. Run one listener per path: two would both answer a dial, and the
dialer would follow only one of them. Both segments start with `.`, so sockets are
[hidden](/concept/moq-lite#hidden-broadcasts) from anyone listing `L`. The
listener discovers dialers with a one-segment watch of `L/.dial/*`, which a
relay serves without scanning everything beneath `L`.

Authorization uses ordinary path grants. A dialer needs publish on
`L/.dial/<id>` and subscribe on `L/.accept/<id>`, and the listener the reverse
for every id: subscribe on `L/.dial/*` and publish on `L/.accept/*`. A grant
naming the id is what stops one dialer from taking another's socket.

## Instances

Each dial is one socket, named by its
[instance](/concept/moq-lite#publisher-instances):

1. The dialer announces `L/.dial/<id>` under a fresh random instance and
   follows `L/.accept/<id>` pinned to that instance.
2. When the application accepts, the listener requests `L/.dial/<id>` pinned
   to the instance it saw announced, then announces `L/.accept/<id>` under the
   same instance at epoch 0.
3. The dialer's follow resolves on that answer, and the socket is open.

An answer to another dial of the same id carries another instance, so a
dialer never takes it. Dialing again with the same id, as a restarted dialer
does, announces a new instance: the listener ends the socket it replaces and
answers the new one on its next accept.

A dial waits for as long as it takes, so give up by dropping it. Nothing reads
a dialer before the application accepts it: dialers wait in the listener's
announcements without limit, and its broadcast is requested only on accept.

## Streams

A stream is a track, named by who opened it, which way it carries data, and
its number:

```text
.<opener>/<kind>/<number>/<name>
```

`<opener>` is `dial` or `accept`, and `<kind>` is `uni` or `bi`. `<number>`
counts the opener's streams in decimal from 0 with no leading zeros, uni and bi
alike, and is never reused within a socket. `<name>` is the application's name
for the stream, which may hold `/` and may repeat: each open is a new stream.

- **Uni.** The opener publishes the track in its own broadcast. The peer
  accepts it and subscribes.
- **Bi.** The opener publishes the track in its own broadcast. The peer accepts
  it by publishing the same name in its broadcast, with the opener's track
  info, and each side subscribes to the other's. The opener's stream resolves
  once the answer is listed.

The opener prefix and the number together work like a QUIC stream ID. The
prefix keeps the two sides' names apart, the way the ID's low bit does, so the
dialer's bi `chat` and the listener's bi `chat` are two streams. The number
keeps one side's opens apart, so a track name always means one stream: a
handle the peer kept to an earlier `chat` never reads a later one. A track's
info, priority included, is the opener's to choose.

The peer learns of new streams from the broadcast's track listing (the Tracks
stream, `TRACKS_ADD`). A listing reports every track the broadcast holds, so
accept skips any name outside the scheme. That leaves applications free to use
fixed track names in either broadcast beside the streams.

A stream can be accepted only while its opener lists it, which holds until the
opener drops the track, finished or not. One dropped before the peer accepts it
is never handed out, so a backlog of unaccepted streams costs the peer no more
than the tracks the opener keeps.

## Ending

A socket ends when either side's broadcast ends, which includes either side
closing it or a session on the way dropping, or when a newer dial of its id
replaces it. Each side then drops its socket, which retracts its broadcast and
ends the socket for the peer. A side that closed opens and accepts nothing
more, and its pending bi opens fail, even when the peer answers them later.

## Versions

Sockets need moq-lite 07 (`moq-lite-07-wip`, opt-in) on every hop: older
moq-lite versions and moq-transport carry neither instances nor track
listings. Over an older hop, accepting a stream fails with `Unsupported`, and
a dial never resolves, because the answer no longer carries the dial's
instance.
