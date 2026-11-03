# Sockets over MoQ

## Goal

`moq-net` gains a generic dial/accept primitive. A `Listener` at a path accepts
the `Socket`s that dialers announce. Inside a socket either side opens and
accepts unnamed streams, as tracks, the way QUIC does. moq-boy, CARLA,
[byte streams](/quest/m2/stream/README.md), and the mesh
[Endpoint](/quest/m2/mesh/endpoint.md) all build on it instead of each
hand-rolling the announce, watch, and request loop. Rust, JS, and Python
(through moq-ffi) expose the same surface and interoperate.

Non-goals: a wire change to moq-lite (a socket is a path and track convention
over existing announce, Tracks, and subscribe streams); session caps, leases,
or idle timeouts, which stay application logic (boy's 60 s idle, CARLA's
one-driver lease and 1 s watchdog); announcement backpressure, which
[announce credit](/quest/m2/announce-credit.md) owns.

## Plan

Decided 2026-11-03 (maintainer, /quest-complete), on
[moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677):

- Copy QUIC's API: socket streams have no application name. Tracks are
  `.<opener>/<kind>/<number>`, with `<opener>` `dial` or `accept`, `<kind>`
  `uni` or `bi`, and `<number>` the opener's never-reused counter. The surface
  is quinn's: `open_uni`, `open_bi`, `accept_uni`, `accept_bi`, each taking
  the track info an open needs. Why: no consumer needs the name. Well-known
  channels (boy's `command`; CARLA's `control`, ack, and latency) are ordinary
  fixed-name tracks on the socket's broadcasts, and Kixel T3 runs byte
  streams with its own framing. Not chosen: a hidden number plus an app name,
  or making name uniqueness the app's job.
- Auth uses moq.pro-style tokens with prefix grants only, rooted at the
  listener's path `L`: the listener subscribes `.dial/**` and publishes
  `.accept/**`, and a dialer publishes `.dial/<id>` and subscribes
  `.accept/<id>`. No mid-path wildcard is needed; it's the shape of moq.pro's
  voice demo (`request/<id>`, `response/<id>`). A relay test with real
  `moq-auth` tokens covers it, and `doc/concept/socket.md` shows the tokens.

Decided 2026-11-02 (maintainer, Kixel planning): the whole epic moves from
m2 to m1, so the next cut publishes sockets in `@moq/net` and moq.pro's
relays carry them. Why: Kixel's T3 Code prototype
([moq-dev/kixel.ai T3 epic](https://github.com/moq-dev/kixel.ai/blob/main/quest/m0/t3/README.md))
sends T3's commands over sockets, one group per message, in place of byte
streams. `@moq/net` publishes from `release`, and accept relies on lite-07's
Tracks stream, which only `main` has, so a backport was declined and the cut
is the route. Byte streams, fastest-reader pacing, and the mesh libraries
keep their own milestones.

Decided 2026-11-01 with the maintainer (planning interview from moq.pro's TCP
replacement arc):

- Name and home: `Listener`, `dial`, and `Socket` in a `socket` module of
  `moq-net` (`moq_net::socket`), the `socket` export of `@moq/net`, and
  moq-ffi beside origin. Not a separate crate: it uses only the origin,
  broadcast, and track APIs every binding already wraps. `Session` is taken by
  the transport connection.
- Layout is owned by the listener and hidden. A listener at `L` keeps `L` for
  whatever it shares with everyone (boy's game, CARLA's cameras), so fanout
  is unchanged. A dialer announces `L/.dial/<id>` and the listener answers on
  `L/.accept/<id>`. Discovery is a one-segment watch of `L/.dial/*`, which is
  cheap for relays, unlike a globstar over the whole origin. Auth enforces the
  id with prefix grants rooted at `L` (see the 2026-11-03 decision). The dot
  keeps sessions out of ordinary discovery
  ([hidden broadcasts](/doc/concept/moq-lite.md#hidden-broadcasts)), so boy's
  game list never sees viewers.
- Identity is a dialer-chosen id plus a pinned instance (the epic first
  called it an epoch; the code's term is the route's `Instance`). The id is the
  dialer's own: an endpoint ID in the mesh, a random id in the demos. Each dial
  instance is one socket: a new instance ends the old socket and starts a new
  one, and the listener's request is pinned to the instance
  (`origin::Consumer::request` with an owned `Instance`, as CARLA already
  does), so a restarted dialer never inherits state. A socket ends when either
  side's broadcast ends or its instance is replaced. Settled by the Rust
  implementation: the listener's answer carries the dial's instance, and the
  dialer follows its answer pinned to it, so it never takes an answer to an
  older dial of its id. A dial waits indefinitely; callers bound it by dropping
  it.
- Streams are tracks. A uni stream is a track the opener publishes in its own
  socket broadcast. A bi stream is the same track name in both. The peer
  learns of new tracks from the Lite07 Tracks stream (`TRACKS_ADD`, `REMOVE`,
  `FINISH`), which pushes them like QUIC's accept. `requested_track` was
  declined because it is pull-shaped: only the side being subscribed to can
  accept. Settled by the Rust implementation and the 2026-11-03 decision:
  tracks are named `.<opener>/<uni|bi>/<number>` with `<opener>` `dial` or
  `accept`, so the two sides' streams never collide, and `<number>` counting
  the opener's streams, so a track never carries two streams; accept skips
  names outside the scheme, so fixed track names coexist; the opener chooses the track info (priority included)
  and a bi answer mirrors it. [`doc/concept/socket.md`](/doc/concept/socket.md)
  is the convention JS and Python follow.
- Accept is unbounded and lazy. Announced dialers queue without limit, and a
  dialer's broadcast is requested only when the application accepts it, so an
  application that stops accepting holds no subscriptions. Backpressure on the
  announcements themselves (paced by the fastest consumer, made hard by merging
  overlapping prefixes) is a separate design, settled as wire-level
  [announce credit](/quest/m2/announce-credit.md).
- The convention is specified on a doc.moq.dev concept page,
  `doc/concept/socket.md`, written with the Rust implementation. It is not a
  section of the moq-lite draft or a new draft: it is an application
  convention over moq-lite.
- The demos move per-client channels onto the socket as fixed-name tracks on
  its broadcasts, not streams: boy's `command` and CARLA's `control` on the
  dialer's `.dial` broadcast, and boy's per-viewer latency and CARLA's driver
  ack and sequence on the listener's `.accept` broadcast. State every viewer
  needs (the game, who is driving, who holds the
  e-stop) stays in the shared broadcast at `L`.

This epic owns removing the hand-rolled loops: after the ports, no
announce-watch-request pairing remains in `rs/moq-boy`, `js/moq-boy`, or
`demo/carla`.

## Required

- [JS sockets](/quest/m1/socket/js.md) - the same surface in `@moq/net`, interoperating with Rust
- [Python sockets](/quest/m1/socket/py.md) - moq-ffi and `py/moq-rs` bindings, interoperating with Rust
- [moq-boy on sockets](/quest/m1/socket/boy.md) - the emulator accepts viewers through a listener and echoes latency per socket
- [CARLA on sockets](/quest/m1/socket/carla.md) - trucks accept drivers through a listener and ack them per socket

## Related

- [Byte streams](/quest/m2/stream/README.md) - TCP over sockets, one bi track per connection
- [Endpoint API](/quest/m2/mesh/endpoint.md) - the mesh's `connect`/`accept`, a socket plus signing
