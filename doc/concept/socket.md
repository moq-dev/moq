---
title: Sockets
description: Dial and accept point-to-point sessions over MoQ, each a reliable, ordered pipe of messages
---

# Sockets

A socket connects two endpoints through relays, the way a TCP connection
connects two hosts. Each endpoint announces one hidden broadcast, dials other
endpoints, and accepts their dials. Each connection is a session: a reliable,
ordered, duplex pipe of messages that only the two ends can read. Use it for
per-client state that only the two ends need, such as a viewer's commands and
the replies to them, while whatever every client needs stays in an ordinary
broadcast.

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
<prefix>/.tcp/<id>
```

The id is one path segment: a well-known name for a server, such as a game, or
a random id for a client. The prefix is shared by every endpoint that talks to
each other. The `.tcp` segment starts with `.`, so endpoints are
[hidden](/concept/moq-lite#hidden-broadcasts) from anyone listing the prefix,
and it names the mode: one reliable, ordered pipe per session, as TCP gives.

Connections are never announced. An endpoint's broadcast holds only session
tracks, served on request, and its announcement is the only thing a relay
routes.

## Sessions

A session between a dialer `A` and an acceptor `B` is named by a session id
`S` that `A` picks: random and unguessable, at least 128 bits, such as 32 hex
digits. Both halves of the session are tracks with the same name,
`<dialer>/<session>`, one in each endpoint's broadcast:

| Track | In broadcast | Written by | Read by |
| --- | --- | --- | --- |
| `A/S` | `<prefix>/.tcp/A` | `A` | `B` |
| `A/S` | `<prefix>/.tcp/B` | `B` | `A` |

The dialer's id leads the name in both broadcasts, so an endpoint tells a dial
from an answer by the name alone: a request under another endpoint's id is that
endpoint dialing, and a request under its own id answers one of its own dials.

### Dial

1. `A` picks `S` and holds its own half, `A/S` in its broadcast, unanswered.
2. `A` subscribes to `<prefix>/.tcp/B` for the track `A/S`, waiting for `B`'s
   endpoint to be announced if it is not yet.
3. `B` receives that subscription as a request for a track it does not have:
   an incoming dial, claiming to be from `A`, with session `S`.

### Accept

1. `B` subscribes to `<prefix>/.tcp/A` for the track `A/S`, and holds `A`'s
   subscription unanswered.
2. `A` answers that subscription only if it dialed `S` and is still waiting,
   and resets any other. That makes the exchange a return-routability check:
   a dial forged in `A`'s name, by anyone who can subscribe to `B`, reaches an
   `A` that never dialed it, so `A` resets `B`'s subscription and `B` resets the
   forger's.
3. Once `A` answers, `B` answers `A`'s subscription, and the session is open
   for both. `B` writes nothing before `A` answers.

### Reject

`B` resets `A`'s subscription, with not found, as for a session that does not
exist. An endpoint that is not accepting dials resets them the same way. A dial
that gives up resets `B`'s subscription in turn.

## Messages

Each side writes its own half and reads the peer's. A message is one group, so a
slow message never holds up the next, and messages are ordered by group
sequence. A reader reads reliably and in order by default: oldest group first,
with no delay budget, so it never skips a message. An application may opt into a
lossy budget for a session whose messages go stale, such as positions, by
setting a delay budget on its subscription.

Finishing a half closes the session that way, like a FIN. Resetting it is an
error. An endpoint closing, or its broadcast ending, resets every session it
holds, and stops reading them. A side that sees the peer's half reset treats
the session as closed and resets its own half, so both halves end.

There is no multiplexing within a session: another channel is another session.
Several sessions between the same two endpoints are independent, and a
restarted endpoint starts new sessions. A channel that every client reads, such
as a game's status, stays an ordinary broadcast.

## Hidden names

The session id is the secret that keeps a session to its two ends: the token
grants below let any endpoint subscribe to any other, so whoever learns a
session's track name can read it, or answer it before its peer. A session
track therefore never appears where a third party can read it:

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
{ "root": "<prefix>", "publish": [".tcp/<id>"], "subscribe": [".tcp/**"] }
```

The publish grant pins the id, so no endpoint can announce as another. The
subscribe grant lets it reach any endpoint, which is what dialing and accepting
need; the session id keeps it out of other endpoints' sessions. A narrower
subscribe grant, such as a client allowed only `.tcp/<server>`, also works: it
can then dial only that server and accept only its dials.

`moq auth sign` writes this as, for example:

```bash
moq auth sign --key key.jwk --root "$PREFIX" --publish ".tcp/$ID" --subscribe '.tcp/**' > endpoint.jwt
```

Each client then connects to the relay at the prefix with its token, and its
endpoint's prefix is the session root.

Signing and encryption are not part of the convention: an endpoint id is a
name the token pins, not a key, and relays are trusted with session names. A
planned mesh layer makes ids public keys.

## Later

A multiplexed mode, with many streams per session like QUIC, would be its own
endpoint, `<prefix>/.quic/<id>`, beside `.tcp`, and could name its streams
beneath the session, such as a control stream under `<dialer>/<session>/`. A
`.tcp` session name holds no `/` after the session id, so it leaves that room.
