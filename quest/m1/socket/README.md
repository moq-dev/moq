# Sockets over MoQ

## Goal

`moq-net` gains a generic dial/accept primitive. An `Endpoint` announces one
hidden broadcast, dials other endpoints, and accepts their dials, and each
connection is a session: one reliable, ordered pipe of messages between the
two ends. moq-boy, CARLA, [byte streams](/quest/m2/stream/README.md), and the
mesh [Endpoint](/quest/m2/mesh/endpoint.md) all build on it instead of each
hand-rolling the announce, watch, and request loop. Rust, JS, and Python
(through moq-ffi) expose the same surface and interoperate.

Non-goals: a wire change to moq-lite (a socket is a path and track convention
over existing announce and subscribe streams); session caps, leases, or idle
timeouts, which stay application logic (boy's 60 s idle, CARLA's one-driver
lease and 1 s watchdog); signing and encryption, which the m2
[mesh](/quest/m2/mesh/README.md) owns by making endpoint ids public keys.

## Plan

[`doc/concept/socket.md`](/doc/concept/socket.md) is the convention JS and
Python follow, and `rs/moq-net/src/model/socket.rs` implements it.

Decided 2026-11-03 (maintainer, /quest-grill in /quest-complete), on
[moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677), replacing the
listener layout and QUIC-style streams below:

- Endpoint sockets, one pipe per session. Each endpoint announces one hidden
  broadcast, `<prefix>/.tcp/<id>`, once, and connections are never announced.
  `<id>` is a well-known name for a server (a game) or a random id for a
  client. Why: the old flow had the listener answer `L/.accept/<id>` under the
  dialer's instance, which broke instance uniqueness (lite-07 treats routes
  sharing an instance as replicas), let two listeners accept one dial,
  announced every connection publicly, and needed lite-07 instances and track
  listings on every hop.
- The segment is `.tcp`, not `.socket`, so the path names the protocol: one
  reliable, ordered pipe per session. A multiplexed mode would be its own
  `.quic/<id>` endpoint later. The Rust and JS module stays `socket`
  (`socket::Endpoint`, `socket::Stream`), since `.tcp` and a later `.quic` are
  modes of one socket API.
- Dial: `A` picks a random, unguessable session id `S` (128 bits) and
  subscribes to `.tcp/<B>`; `B` surfaces the requested track as an incoming
  dial claiming to be from `A`. Accept: `B` subscribes to `.tcp/<A>`, and `A`
  answers only a session it dialed and resets any other, a return-routability
  check that fails a dial forged in `A`'s name. `B` sends nothing until `A`
  answers. Reject: `B` resets `A`'s subscription.
- The pipe is one track each way. One message per group, so messages don't
  block each other, ordered by group sequence; reliable and in order by
  default (no delay budget, ascending), and the app may opt into a lossy
  budget. Finishing a track closes the pipe, resetting it is an error, and the
  endpoint's broadcast ending disconnects.
- No multiplexing: another channel is another session, a pair of endpoints
  holds several, and a restart is a new session. No control track and no
  numbered streams now.
- Session track names are the secret, so they never appear in a lite-07 track
  listing or anywhere else a third party reads. moq-net already lists only
  tracks a producer creates or a handler's `Lister` answers, never one served
  on request, so an endpoint serves its session tracks only on request. The
  dialer's own half is reserved unlisted
  (`broadcast::Producer::reserve_unlisted_track`, crate-private).
- Grants are prefix only, rooted at the prefix: publish `.tcp/<self>`,
  subscribe `.tcp/**`, or narrower subscribes. A relay test signs real
  `moq-auth` tokens.
- It works over every moq-lite version and moq-transport, since the handshake
  uses no instances or listings; lite-07 instances only affect relay route
  choice. Tests cross moq-transport hops.
- Fixed channels become sessions or messages on one: boy's `command` and
  per-viewer latency, CARLA's `control`, ack, and e-stop. Shared state such as
  boy's `status` stays an ordinary broadcast.
- `socket::Stream` is a plain struct with public `producer` (this side's half)
  and `consumer` (the peer's half) fields, plus read-only `peer()` and
  `session()`, and no read, write, or message methods of its own: apps use the
  track API directly (groups as messages, priority, budget), so JS, Python, and
  the bindings mirror a minimal surface.

Settled by the Rust implementation of that decision, for the maintainer to
confirm (see the PR's Waiting section):

- Both halves are named `<dialer>/<session>`, not each after its reader. With
  reader-named halves, an endpoint that both dials and accepts cannot tell a
  peer's dial from a peer's answer, so it could not reset an answer to a
  session it never dialed: a forged dial surfaced at the claimed dialer as a
  dial from the victim and deadlocked both accepts. With the dialer's id
  leading both names, a request under an endpoint's own id is always an answer.
- `Stream::consumer` is a `track::Subscriber`, already subscribed reliably and
  in order, not a bare `track::Consumer`, whose default subscription is lossy
  and newest first. The handshake's subscription becomes the stream's, so no
  message falls between the accept and the app's first read; `update` makes it
  lossy.
- An endpoint takes dials from its first `accept` on, like a TCP listener; until
  then it has no request handler, so a dial to it is reset at once. A dial
  reserves its own half, which the peer's answer finds without a handler, so a
  dial-only endpoint never needs polling. A dial waits for the peer's endpoint
  to be announced (drop it to give up); an accept fails at once when the
  claimed dialer's endpoint is not announced.
- Accepted dials queue without limit while the app accepts, each holding the
  dialer's subscription open. Announce backpressure is no longer a socket
  concern, since connections are never announced.

Decided 2026-11-02 (maintainer, Kixel planning): the whole epic moves from
m2 to m1, so the next cut publishes sockets in `@moq/net` and moq.pro's
relays carry them. Why: Kixel's T3 Code prototype
([moq-dev/kixel.ai T3 epic](https://github.com/moq-dev/kixel.ai/blob/main/quest/m0/t3/README.md))
sends T3's commands over sockets, one group per message, in place of byte
streams. `@moq/net` publishes from `release`. Byte streams, fastest-reader
pacing, and the mesh libraries keep their own milestones.

Decided 2026-11-01 with the maintainer (planning interview from moq.pro's TCP
replacement arc), still standing:

- Home: a `socket` module of `moq-net` (`moq_net::socket`), the `socket`
  export of `@moq/net`, and moq-ffi beside origin. Not a separate crate: it
  uses only the origin, broadcast, and track APIs every binding already wraps.
- The convention is specified on a doc.moq.dev concept page,
  `doc/concept/socket.md`, not in the moq-lite draft: it is an application
  convention over moq-lite.

Later, not built now: a multiplexed `.quic/<id>` mode with many streams per
session, which could name a control stream and numbered streams beneath the
session (`<dialer>/<session>/...`); and signed or encrypted sessions with ids
as public keys, in the m2 [mesh](/quest/m2/mesh/README.md).

This epic owns removing the hand-rolled loops: after the ports, no
announce-watch-request pairing remains in `rs/moq-boy`, `js/moq-boy`, or
`demo/carla`.

## Required

- [JS sockets](/quest/m1/socket/js.md) - the same surface in `@moq/net`, interoperating with Rust
- [Python sockets](/quest/m1/socket/py.md) - moq-ffi and `py/moq-rs` bindings, interoperating with Rust
- [moq-boy on sockets](/quest/m1/socket/boy.md) - viewers dial the emulator's endpoint and send commands and receive latency on their session
- [CARLA on sockets](/quest/m1/socket/carla.md) - drivers dial the truck's endpoint and send control and receive acks on their session

## Related

- [Byte streams](/quest/m2/stream/README.md) - TCP over sockets, one session per connection
- [Endpoint API](/quest/m2/mesh/endpoint.md) - the mesh's `connect`/`accept`, a socket endpoint plus signing
