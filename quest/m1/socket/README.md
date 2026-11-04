# Sockets over MoQ

## Goal

`moq-net` gains a generic dial/accept primitive. An `Endpoint` announces one
hidden broadcast, dials other endpoints, and accepts their dials, and each
connection is a session between the two ends: a reliable, ordered byte pipe
each way (`tcp`), or a track of messages each way (`track`). moq-boy, CARLA, [byte streams](/quest/m2/stream/README.md), and the
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

Decided 2026-11-04, second round (maintainer, settling the Waiting items of
[moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677)):

- Name sessions `<protocol>/<dialer>/<acceptor>/<S>`, binding a session to both
  ends. An endpoint resets a dial naming another acceptor, and the dialer
  serves only the name it dialed, so an endpoint `A` dialed can't replay the
  session to a third endpoint posing as `A`. Done now, before the JS and Python
  ports copy the scheme, rather than waiting for the m2 mesh keys. Not chosen:
  keep `<dialer>/<S>` and document the replay.
- An accept waits for the claimed dialer's endpoint for as long as the dial
  stays open, with no timer: a relay can deliver a dial before the dialer's
  announcement, such as when two endpoints dial each other at start-up. A
  forged dial claiming an endpoint nobody announces holds an accept only while
  the forger holds its subscription. Not chosen: fail at once and let the
  dialer redial; a bounded timer.

Decided 2026-11-04 (maintainer, /quest-complete), on
[moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677), settling its
Waiting items and replacing the `.tcp/<id>` layout and the one-message-per-group
pipe below; the second round above adds the acceptor to the session name:

- Name the dialer in the session. A dial and an answer can't be confused,
  the claimed dialer is named for the return-routability check, and `S`
  (random per dial) makes every session unique, so one endpoint can dial and
  accept any number of sessions. Not chosen: reader-named halves.
- One announcement per endpoint, used only for routing:
  `<prefix>/.socket/<id>`. The session track name carries the protocol, both
  ends and the session: `<protocol>/<dialer>/<acceptor>/<S>`. A dial is
  SUBSCRIBE `<prefix>/.socket/<peer>`, track `tcp/<self>/<peer>/<S>`; the
  answer is the same name in the dialer's broadcast. A requested track whose protocol the
  endpoint doesn't speak, or isn't accepting, is reset as unsupported. Grants:
  publish `.socket/<self>`, subscribe `.socket/**`. The module stays `socket`.
- Two protocols ship now over the same handshake, each with its own dial and
  accept, and an incoming dial lands in its protocol's queue:
  - `tcp/<dialer>/<acceptor>/<S>`: a session half is a single group, not a track of
    messages. A group is one QUIC stream on every hop, so it's reliable,
    strictly ordered and flow controlled, like a TCP direction. Frames are
    writes, finishing the group is FIN, and resetting it is RST. There are no
    track-level settings (budget, max-delay, order), since the newest group is
    never stale. A second group on a session track is a protocol error that
    resets the session. Rust: `Endpoint::dial_tcp` / `accept_tcp`, yielding
    `socket::TcpStream { producer: group::Producer, consumer: group::Consumer }`
    plus `peer()` and `session()`. Not chosen: `track::Ordered` with one message
    per group.
  - `track/<dialer>/<acceptor>/<S>`: the same dial, accept, reject and return-routability
    check, but the app gets the whole session tracks, with groups as messages
    and its own priority, budget and order, and no single-group rule. The
    reader starts reliable and ascending (no delay budget) until the app
    updates it. A half has no max age unless the app sets one, so a newer
    message never makes an older one stale. Rust: `Endpoint::dial_track` /
    `accept_track`, yielding
    `socket::TrackStream { producer: track::Producer, consumer: track::Subscriber }`.
- Long-lived `tcp` sessions rely on unbounded groups (since
  [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411) a group has no
  size cap, and each copy keeps a ~32 MiB tail). With one reader per copy,
  [fastest-reader pacing](/quest/m1/unbounded-groups/fastest-reader.md) acts
  as TCP backpressure: a writer awaiting `group::Producer::ready` runs at most
  16 MiB past its reader on each hop, and a reader that falls off the tail
  fails with `Evicted` rather than skipping data. Where pacing doesn't reach
  yet, that `Evicted` is the failure mode, never silent loss: browsers and JS
  read unpaced ([JS fastest-reader pacing](/quest/m2/js-fastest-reader.md)),
  and a session past its paused-credit cap (about two paused group streams at
  the client defaults) reads and sheds instead of pausing. That remaining
  pacing work stays in the [unbounded groups](/quest/m1/unbounded-groups/README.md)
  epic, which this depends on for lossless `tcp` in those cases.

Implementation choices in
[moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677):

- A `tcp` session runs a small task on the endpoint origin's driver. It copies
  the peer's group into the group the app reads, a frame at a time and paced by
  that reader, so it can end that group itself: on a second group from the
  peer, on the endpoint closing, or when the peer resets. It also finishes or
  aborts this side's track the way the app ended its group, and resets the
  session when the app drops its reader early. Once group 0 arrives, it
  watches for a second group through the subscriber's largest position rather
  than `poll_recv_group`: polling `poll_recv_group` on every wake while
  reading the group it handed out stalled the group's delivery (see the PR).
- `TrackStream::consumer` is the handshake's own `track::Subscriber`, not a bare
  `track::Consumer`: it is already subscribed reliably and in order, so no
  message falls between the accept and the app's first read, and the app
  overrides it with `update` or reads in sequence through `ordered()`.
- An endpoint takes dials in a protocol from its first accept in it, like a TCP
  listener. Until its first accept in any protocol it has no request handler,
  so a dial to it is reset with not found at once. A dial reserves its own
  half, which the peer's answer finds without a handler, so a dial-only
  endpoint never needs polling. A dial waits for the peer's endpoint to be
  announced (drop it to give up), and an accept waits for the claimed
  dialer's endpoint until the dialer drops its subscription.
- Accepted dials queue while the app accepts, each holding the dialer's
  subscription open. A queued dial whose dialer gave up leaves its queue on the
  endpoint's next poll, which each new dial triggers, so a queue holds only
  open dials, even in a protocol the app stopped accepting.
- Session track names are the secret, so they never appear in a lite-07 track
  listing or anywhere else a third party reads. moq-net lists only tracks a
  producer creates or a handler's `Lister` answers, never one served on
  request, so an endpoint serves its session tracks only on request, and the
  dialer's own half is reserved unlisted
  (`broadcast::Producer::reserve_unlisted_track`, crate-private).

Decided 2026-11-03 (maintainer, /quest-grill in /quest-complete), still
standing where the 2026-11-04 decisions above don't replace it:

- Endpoint sockets, one session per connection. Each endpoint announces one
  hidden broadcast once, and connections are never announced. `<id>` is a
  well-known name for a server (a game) or a random id for a client. Why: the
  old flow had the listener answer `L/.accept/<id>` under the dialer's
  instance, which broke instance uniqueness (lite-07 treats routes sharing an
  instance as replicas), let two listeners accept one dial, announced every
  connection publicly, and needed lite-07 instances and track listings on
  every hop.
- Dial: `A` picks a random, unguessable session id `S` (128 bits) and
  subscribes to `B`'s endpoint; `B` surfaces the requested track as an
  incoming dial claiming to be from `A`. Accept: `B` subscribes to `A`'s
  endpoint, and `A` answers only a session it dialed and resets any other, a
  return-routability check that fails a dial forged in `A`'s name. `B` sends
  nothing until `A` answers. Reject: `B` resets `A`'s subscription.
- No multiplexing: another channel is another session, a pair of endpoints
  holds several, and a restart is a new session.
- Grants are prefix only, rooted at the prefix. A relay test signs real
  `moq-auth` tokens.
- The handshake uses no instances or listings; lite-07 instances only affect
  relay route choice. Tests cross moq-transport hops. Corrected 2026-11-04 in
  [moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677): it needs
  moq-lite-07 or moq-transport on every hop, since a moq-lite hop before 07
  has no hidden opt-in and never carries an endpoint's `.socket/<id>`
  broadcast. Relays' cluster links carry hidden broadcasts on every version,
  so an endpoint's own client session is the hop that decides: while one is
  moq-lite before 07, its dials and accepts fail `Unsupported` instead of
  waiting ([moq-dev/moq#7888](https://github.com/moq-dev/moq/pull/7888)).
- Fixed channels become sessions or messages on one: boy's `command` and
  per-viewer latency, CARLA's `control`, ack, and e-stop. Shared state such as
  boy's `status` stays an ordinary broadcast.
- The stream types are plain structs with public `producer` (this side's half)
  and `consumer` (the peer's half) fields, plus read-only `peer()` and
  `session()`, and no read, write, or message methods of their own: apps use
  the group and track APIs directly, so JS, Python, and the bindings mirror a
  minimal surface.

Decided 2026-11-02 (maintainer, Kixel planning): the whole epic moves from
m2 to m1, so the next cut publishes sockets in `@moq/net` and moq.pro's
relays carry them. Why: Kixel's T3 Code prototype (its T3 epic, in the
private kixel.ai repo) sends T3's commands over sockets, one group per message,
in place of byte streams. `@moq/net` publishes from `release`. Byte streams,
fastest-reader pacing, and the mesh libraries keep their own milestones.

Decided 2026-11-01 with the maintainer (planning interview from moq.pro's TCP
replacement arc), still standing:

- Home: a `socket` module of `moq-net` (`moq_net::socket`), the `socket`
  export of `@moq/net`, and moq-ffi beside origin. Not a separate crate: it
  uses only the origin, broadcast, and track APIs every binding already wraps.
- The convention is specified on a doc.moq.dev concept page,
  `doc/concept/socket.md`, not in the moq-lite draft: it is an application
  convention over moq-lite.

Later, not built now: a multiplexed `quic` protocol, `quic/<dialer>/<acceptor>/<S>` on
the same endpoint with no new announcement, which brings back several groups
or streams per session; and signed or encrypted sessions with ids as public
keys, in the m2 [mesh](/quest/m2/mesh/README.md).

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
