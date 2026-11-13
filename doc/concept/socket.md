---
title: Sockets
description: Dial and accept point-to-point sessions over MoQ, as reliable byte pipes or as tracks of messages
---

# Sockets

A socket connects two endpoints through relays, the way a TCP connection
connects two hosts. A server accepts dials and a client dials; an endpoint may
be both. Each role an endpoint takes is one hidden broadcast, so ordinary
grants decide who may be a server. Each connection is a session that only the
two ends can read while its id stays secret (see [Hidden names](#hidden-names)),
in one of two protocols: `tcp`, a reliable, ordered byte
pipe each way, or `track`, a track each way whose groups are messages. Use a
socket for per-client state that only the two ends need, such as a viewer's
commands and the replies to them, while whatever every client needs stays in an
ordinary broadcast.

A socket is a convention over announcements, broadcasts, and track
subscriptions, not a wire extension. It uses no instances and no track
listings, and relays carry it unchanged. It needs moq-lite-07 or
moq-transport on every hop: an endpoint's broadcasts are
[hidden](/concept/moq-lite#hidden-broadcasts), and an older moq-lite hop
never carries hidden broadcasts, so a dial across one would never find its
peer. Clustered relays carry hidden broadcasts to each other on every version,
so the hop that decides is each endpoint's own session: while an endpoint's
client session is moq-lite before 07, its dials and accepts fail with
`Unsupported` instead of waiting, unless that session's publish half opts into
hidden broadcasts. A dialer can't tell a peer behind such a hop from one not
online yet, so its dial waits as for any absent peer, while the peer's own
accepts fail. This page is the convention, enough to
implement it in any language.
[`moq_net::socket`](https://docs.rs/moq-net/latest/moq_net/socket/) implements
it in Rust.

## Endpoints

An endpoint with id `<id>` announces one broadcast for each role it takes:

```text
<prefix>/.server/<id>    a server, which peers dial
<prefix>/.client/<id>    a client, which dials servers
```

| Role | Publishes | Subscribes |
| --- | --- | --- |
| Server | `.server/<id>` | `.client/**` |
| Client | `.client/<id>` | `.server/**` |

A server announces its broadcast at its first accept, and a client at its
first dial, so a pure client never announces a server broadcast. An endpoint
that both dials and accepts announces both broadcasts under one id.

The id is one path segment: a well-known name for a server, such as a game, or
a random id for a client. The prefix is shared by every endpoint that talks to
each other. Both role segments start with `.`, so endpoints are
[hidden](/concept/moq-lite#hidden-broadcasts) from anyone listing the prefix.

The announcements are used only for routing: they are how a relay finds an
endpoint. Connections are never announced. An endpoint's broadcasts hold only
session tracks, served on request, whatever their protocol.

## Sessions

A session between a dialer `A` and an acceptor `B` is named by its protocol `P`,
both ends, and a session id `S` that `A` picks: random and unguessable, at
least 128 bits, such as 32 hex digits. Both halves of the session are tracks
with the same name, `<protocol>/<dialer>/<acceptor>/<session>`, one in the
dialer's client broadcast and one in the acceptor's server broadcast:

| Track | In broadcast | Written by | Read by |
| --- | --- | --- | --- |
| `P/A/B/S` | `<prefix>/.client/A` | `A` | `B` |
| `P/A/B/S` | `<prefix>/.server/B` | `B` | `A` |

The protocol is `tcp` or `track`; see [Protocols](#protocols). A server resets
a dial whose protocol it doesn't speak, or isn't accepting, as unsupported.

A request on a server broadcast is a dial, and one on a client broadcast
answers a dial. A server resets a dial that doesn't name another endpoint as
the dialer and itself as the acceptor, such as one naming another acceptor, as
not found, and a client resets any request but the answer to a dial it holds
the same way. Since `S` is random per dial, every session is distinct, and one
endpoint can dial and accept any number of sessions at once, with the same peer
or others.

### Dial

1. `A` picks `S` and holds its own half, `P/A/B/S` in `<prefix>/.client/A`,
   unanswered, announcing that broadcast if this is its first dial.
2. `A` subscribes to `<prefix>/.server/B` for the track `P/A/B/S`, waiting for
   `B`'s server endpoint to be announced if it is not yet.
3. `B` receives that subscription as a request for a track it does not have:
   an incoming dial in protocol `P`, claiming to be from `A`, with session `S`.

### Accept

1. `B` subscribes to `<prefix>/.client/A` for the track `P/A/B/S`, and holds
   `A`'s subscription unanswered. A relay can deliver the dial before `A`'s
   announcement, so `B` waits for `A`'s client endpoint to be announced for as
   long as `A`'s subscription stays open, and gives up once it closes.
2. `A` answers that subscription only if it dialed `S` to `B` and is still
   waiting, and resets any other. That makes the exchange a return-routability
   check: a dial forged in `A`'s name, by anyone who can subscribe to `B`'s
   server endpoint, reaches an `A` that never dialed it, so `A` resets `B`'s
   subscription and `B` resets the forger's.
3. Once `A` answers, `B` answers `A`'s subscription, and the session is open
   for both. `B` writes nothing before `A` answers.

### Reject

`B` resets `A`'s subscription, with not found, as for a session that does not
exist. A server resets a dial in a protocol it is not accepting as unsupported.
An endpoint that has never accepted has no server endpoint, so a dial to it
waits, as for one not online yet. A dial that gives up resets `B`'s
subscription in turn.

## Protocols

Each side writes its own half and reads the peer's. A reader subscribes
reliably and in order: from the half's first frame, oldest first, with no delay
budget, so it waits for a late group rather than skipping it. The protocol
decides what a half holds.

### `tcp`

Each half is exactly one group, sequence 0. In MoQ a group travels as one QUIC
stream on every hop, so it is reliable, strictly ordered, and flow controlled,
exactly like one direction of a TCP connection:

- A frame is a write. The reader reads the frames in the order they were
  written, with nothing skipped.
- The reader joins the live group, which is the half's only one, from its
  first frame. Over moq-transport, that is a `SUBSCRIBE` plus a joining `FETCH`
  (or a fill) for the frames written before its answer.
- Finishing the group is FIN: the reader reads to the end, then sees the end.
- Resetting the group, or its track, is RST: the reader fails with the error.
- A second group on a session track is a protocol error: the reader resets the
  session, both halves.

There are no track-level settings: no priority between messages, no delay
budget, no order, since the newest group is never stale.

A session can last indefinitely: a group has no size cap, and each copy keeps
only a bounded tail of it. A writer that waits for room before each write (in
Rust, `group::Producer::ready`; in the bindings, `GroupProducer.ready(size)`) runs at most a window ahead of the reader on
every hop, so a slow reader slows the writer, as TCP backpressure does, and
nothing is lost. A writer that doesn't wait sheds the front of its half, and a
reader that falls behind it fails with an eviction error instead of skipping
data. Pacing reaches across native QUIC and qmux hops; see
[unbounded groups](https://github.com/moq-dev/moq/tree/main/quest/m3/unbounded-groups)
for what remains, such as browsers, which read without pacing.

Use `tcp` for a byte stream or a log of records that must all arrive in order,
such as a proxied TCP connection, a file transfer, or a request and its reply.

### `track`

Each half is a whole track, and a message is one group. A slow message never
holds up the next, and each group can carry its own priority. Messages are
ordered by group sequence. A half has no max age unless its writer sets one, so
a newer message never makes an older one stale. A reader that keeps up gets
every message, but each
hop's cache reclaims a message nobody read within its idle window (30 s by
default), or under memory pressure, except the newest, so a reader that stalls
longer can miss messages. Any message a hop reports it will never deliver is
skipped the same way, rather than ending the session. Over moq-transport, the
reader starts at group 0: a `SUBSCRIBE` with no filter, plus one range `FETCH`
for the groups below the answer's largest, so a publisher that refuses that
`FETCH` leaves a burst written before the answer missed.

The reader's subscription starts reliable and in order, like `tcp`. An
application may make it lossy for messages that go stale, such as positions or
controls, by updating the subscription with a finite delay budget: a message
that is late past the budget is skipped rather than holding up later ones. It
may also read newest first.

Finishing a half closes the session that way. Resetting it is an error.

Use `track` for independent messages, where a late one should not block the
next: commands, controls, acknowledgements, or telemetry.

## Closing

An endpoint closing, or either of its broadcasts ending, resets every session
it holds, and stops reading them. A side that sees the peer's half reset treats the
session as closed and resets its own half, so both halves end.

There is no multiplexing within a session: another channel is another session.
Several sessions between the same two endpoints are independent, and a
restarted endpoint starts new sessions. A channel that every client reads, such
as a game's status, stays an ordinary broadcast.

## Hidden names

The session id is the secret that keeps a session to its two ends: the token
grants below let every client subscribe to every server, and every server to
every client, so whoever learns a session's track name can read it, or answer
it before its peer.

The acceptor learns the name too, but cannot replay it to a third endpoint
`C`: the name binds both ends. `C` resets `P/A/B/S` as a session for another
acceptor, and `A` never dialed `P/A/C/S`, so it resets `C`'s accept of that.
The peer id of an accepted session is the endpoint that dialed it to this one.

A session track therefore never appears where a third party can read it:

- It is never announced.
- It is never listed: an endpoint serves its session tracks only on request,
  so a moq-lite 07 track listing (`TRACKS`) of either of an endpoint's
  broadcasts names none of them, even to a reader allowed everything.
- Logs and metrics that a third party can read must not carry it.

## Authorization

Sockets need nothing beyond ordinary
[token grants](/bin/relay/auth#path-matching). With the token's root at the
socket prefix, each role is two prefix grants: publish the endpoint's own
broadcast, and subscribe to the other role's.

```jsonc
// The server <id>: publishes its endpoint, reads every client's.
{ "root": "<prefix>", "publish": [".server/<id>"], "subscribe": [".client/**"] }
// The client <id>: publishes its endpoint, reads every server's.
{ "root": "<prefix>", "publish": [".client/<id>"], "subscribe": [".server/**"] }
```

An endpoint that both dials and accepts holds both. The publish grant pins the
role and the id, so a client can't announce a server endpoint, or announce as
another endpoint, and a client grant reads no other client's endpoint. The
subscribe grant lets an endpoint reach every peer of the other role, which is
what dialing and accepting need; the session id keeps it out of other
endpoints' sessions. A narrower subscribe grant, such as a client allowed only
`.server/<server>`, also works: it can then dial only that server.

A grant shared by many clients, such as an anonymous one publishing
`.client/*`, lets each announce any client id, so a client's id must stay a
secret too: random, and never published, such as in a status track. Two
announcements of one path coexist, so an impostor announcing a client's id can
break that client's next dial, though not read or join its sessions.

`moq auth sign` writes these as, for example:

```bash
moq auth sign --key key.jwk --root "$PREFIX" --publish ".server/$ID" --subscribe '.client/**' > server.jwt
moq auth sign --key key.jwk --root "$PREFIX" --publish ".client/$ID" --subscribe '.server/**' > client.jwt
```

Each client then connects to the relay at the prefix with its token, and its
endpoint's prefix is the session root.

Signing and encryption are not part of the convention: an endpoint id is a
name the token pins, not a key, and relays are trusted with session names. A
planned mesh layer makes ids public keys.

## Later

Later protocols are other prefixes on the same endpoint broadcasts, with no
new announcement. A multiplexed `quic` mode, with many streams per session like
QUIC, would name its sessions `quic/<dialer>/<acceptor>/<session>` and bring back several
groups or streams per session.
