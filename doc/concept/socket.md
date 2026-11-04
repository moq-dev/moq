---
title: Sockets
description: Dial and accept point-to-point sessions over MoQ, as reliable byte pipes or as tracks of messages
---

# Sockets

A socket connects two endpoints through relays, the way a TCP connection
connects two hosts. Each endpoint announces one hidden broadcast, dials other
endpoints, and accepts their dials. Each connection is a session that only the
two ends can read while its id stays secret (see [Hidden names](#hidden-names)),
in one of two protocols: `tcp`, a reliable, ordered byte
pipe each way, or `track`, a track each way whose groups are messages. Use a
socket for per-client state that only the two ends need, such as a viewer's
commands and the replies to them, while whatever every client needs stays in an
ordinary broadcast.

A socket is a convention over announcements, broadcasts, and track
subscriptions, not a wire extension. It uses no instances and no track
listings, so it works over every moq-lite version and moq-transport, and relays
carry it unchanged. This page is the convention, enough to implement it in any
language.
[`moq_net::socket`](https://docs.rs/moq-net/latest/moq_net/socket/) implements
it in Rust.

## Endpoints

An endpoint with id `<id>` announces one broadcast, once:

```text
<prefix>/.socket/<id>
```

The id is one path segment: a well-known name for a server, such as a game, or
a random id for a client. The prefix is shared by every endpoint that talks to
each other. The `.socket` segment starts with `.`, so endpoints are
[hidden](/concept/moq-lite#hidden-broadcasts) from anyone listing the prefix.

The announcement is used only for routing: it is how a relay finds an
endpoint. Connections are never announced. An endpoint's broadcast holds only
session tracks, served on request, whatever their protocol.

## Sessions

A session between a dialer `A` and an acceptor `B` is named by its protocol `P`
and a session id `S` that `A` picks: random and unguessable, at least 128 bits,
such as 32 hex digits. Both halves of the session are tracks with the same
name, `<protocol>/<dialer>/<session>`, one in each endpoint's broadcast:

| Track | In broadcast | Written by | Read by |
| --- | --- | --- | --- |
| `P/A/S` | `<prefix>/.socket/A` | `A` | `B` |
| `P/A/S` | `<prefix>/.socket/B` | `B` | `A` |

The protocol is `tcp` or `track`; see [Protocols](#protocols). An endpoint
resets a request whose protocol it doesn't speak, or isn't accepting, as
unsupported.

The dialer's id follows the protocol in both broadcasts, so an endpoint tells a
dial from an answer by the name alone: a request under another endpoint's id is
that endpoint dialing, and a request under its own id answers one of its own
dials. Since `S` is random per dial, every session is distinct, and one
endpoint can dial and accept any number of sessions at once, with the same peer
or others.

### Dial

1. `A` picks `S` and holds its own half, `P/A/S` in its broadcast, unanswered.
2. `A` subscribes to `<prefix>/.socket/B` for the track `P/A/S`, waiting for
   `B`'s endpoint to be announced if it is not yet.
3. `B` receives that subscription as a request for a track it does not have:
   an incoming dial in protocol `P`, claiming to be from `A`, with session `S`.

### Accept

1. `B` subscribes to `<prefix>/.socket/A` for the track `P/A/S`, and holds
   `A`'s subscription unanswered.
2. `A` answers that subscription only if it dialed `S` and is still waiting,
   and resets any other. That makes the exchange a return-routability check:
   a dial forged in `A`'s name, by anyone who can subscribe to `B`, reaches an
   `A` that never dialed it, so `A` resets `B`'s subscription and `B` resets the
   forger's. A dial replayed from a session `A` really dialed passes; see
   [Hidden names](#hidden-names).
3. Once `A` answers, `B` answers `A`'s subscription, and the session is open
   for both. `B` writes nothing before `A` answers.

### Reject

`B` resets `A`'s subscription, with not found, as for a session that does not
exist. An endpoint resets a dial in a protocol it is not accepting as
unsupported, and one that accepts nothing at all answers not found, as for any
track nobody serves. A dial that gives up resets `B`'s subscription in turn.

## Protocols

Each side writes its own half and reads the peer's. A reader subscribes
reliably and in order: oldest group first, with no delay budget, so it waits
for a late group rather than skipping it. The protocol decides what a half
holds.

### `tcp`

Each half is exactly one group, sequence 0. In MoQ a group travels as one QUIC
stream on every hop, so it is reliable, strictly ordered, and flow controlled,
exactly like one direction of a TCP connection:

- A frame is a write. The reader reads the frames in the order they were
  written, with nothing skipped.
- Finishing the group is FIN: the reader reads to the end, then sees the end.
- Resetting the group, or its track, is RST: the reader fails with the error.
- A second group on a session track is a protocol error: the reader resets the
  session, both halves.

There are no track-level settings: no priority between messages, no delay
budget, no order, since the newest group is never stale.

A session can last indefinitely: a group has no size cap, and each copy keeps
only a bounded tail of it. A writer that waits for room before each write (in
Rust, `group::Producer::ready`) runs at most a window ahead of the reader on
every hop, so a slow reader slows the writer, as TCP backpressure does, and
nothing is lost. A writer that doesn't wait sheds the front of its half, and a
reader that falls behind it fails with an eviction error instead of skipping
data. Pacing reaches across native QUIC and qmux hops; see
[unbounded groups](https://github.com/moq-dev/moq/tree/main/quest/m1/unbounded-groups)
for what remains, such as browsers, which read without pacing.

Use `tcp` for a byte stream or a log of records that must all arrive in order,
such as a proxied TCP connection, a file transfer, or a request and its reply.

### `track`

Each half is a whole track, and a message is one group. A slow message never
holds up the next, and each group can carry its own priority. Messages are
ordered by group sequence. A reader that keeps up gets every message, but each
hop's cache reclaims a message nobody read within its idle window (30 s by
default), or under memory pressure, except the newest, so a reader that stalls
longer can miss messages.

The reader's subscription starts reliable and in order, like `tcp`. An
application may make it lossy for messages that go stale, such as positions or
controls, by updating the subscription with a finite delay budget: a message
that is late past the budget is skipped rather than holding up later ones. It
may also read newest first.

Finishing a half closes the session that way. Resetting it is an error.

Use `track` for independent messages, where a late one should not block the
next: commands, controls, acknowledgements, or telemetry.

## Closing

An endpoint closing, or its broadcast ending, resets every session it holds,
and stops reading them. A side that sees the peer's half reset treats the
session as closed and resets its own half, so both halves end.

There is no multiplexing within a session: another channel is another session.
Several sessions between the same two endpoints are independent, and a
restarted endpoint starts new sessions. A channel that every client reads, such
as a game's status, stays an ordinary broadcast.

## Hidden names

The session id is the secret that keeps a session to its two ends: the token
grants below let any endpoint subscribe to any other, so whoever learns a
session's track name can read it, or answer it before its peer.

That includes the acceptor. The name says who dialed, but not whom, so the
return-routability check proves only that `A` dialed `S`, not that it dialed
`B`. An endpoint `A` dialed knows `P/A/S`, and can dial a third endpoint `C`
with it: `C`'s accept finds `A` serving that half, so `C` accepts a session
that claims to be from `A`, reads what `A` writes to the replaying endpoint,
and sends its replies to that endpoint. Until ids are keys, the peer id of an
accepted session says only that the peer dialed `S`, to somebody.

A session track therefore never appears where a third party can read it:

- It is never announced.
- It is never listed: an endpoint serves its session tracks only on request,
  so a moq-lite 07 track listing (`TRACKS`) of an endpoint's broadcast names
  none of them, even to a reader allowed everything.
- Logs and metrics that a third party can read must not carry it.

## Authorization

Sockets need nothing beyond ordinary
[token grants](/bin/relay/auth#path-matching). With the token's root at the
socket prefix, each grant is a prefix:

```jsonc
// The endpoint <id>: publishes its own endpoint, reads every endpoint.
{ "root": "<prefix>", "publish": [".socket/<id>"], "subscribe": [".socket/**"] }
```

The publish grant pins the id, so no endpoint can announce as another. The
subscribe grant lets it reach any endpoint, which is what dialing and accepting
need; the session id keeps it out of other endpoints' sessions. A narrower
subscribe grant, such as a client allowed only `.socket/<server>`, also works:
it can then dial only that server and accept only its dials.

`moq auth sign` writes this as, for example:

```bash
moq auth sign --key key.jwk --root "$PREFIX" --publish ".socket/$ID" --subscribe '.socket/**' > endpoint.jwt
```

Each client then connects to the relay at the prefix with its token, and its
endpoint's prefix is the session root.

Signing and encryption are not part of the convention: an endpoint id is a
name the token pins, not a key, and relays are trusted with session names. A
planned mesh layer makes ids public keys.

## Later

Later protocols are other prefixes on the same endpoint, with no new
announcement. A multiplexed `quic` mode, with many streams per session like
QUIC, would name its sessions `quic/<dialer>/<session>` and bring back several
groups or streams per session.
