# Sockets over MoQ

## Goal

`moq-net` gains a generic dial/accept primitive. A `Listener` at a path accepts
the `Socket`s that dialers announce. Inside a socket either side opens and
accepts named tracks the way QUIC opens and accepts streams. moq-boy, CARLA,
[byte streams](/quest/m2/stream/README.md), and the mesh
[Endpoint](/quest/m2/mesh/endpoint.md) all build on it instead of each
hand-rolling the announce, watch, and request loop. Rust, JS, and Python
(through moq-ffi) expose the same surface and interoperate.

Non-goals: a wire change to moq-lite (a socket is a path and track convention
over existing announce, Tracks, and subscribe streams); session caps, leases,
or idle timeouts, which stay application logic (boy's 60 s idle, CARLA's
one-driver lease and 1 s watchdog); announcement backpressure, which is planned
separately.

## Plan

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
  id with a mid-path wildcard grant (`mesh/*/*/.dial/<id>`), which
  `moq-pattern` supports. The dot keeps sessions out of ordinary discovery
  ([hidden broadcasts](/doc/concept/moq-lite.md#hidden-broadcasts)), so boy's game list
  never sees viewers.
- Identity is a dialer-chosen id plus a pinned epoch. The id is the dialer's
  own: an endpoint ID in the mesh, a random id in the demos. Each announce
  epoch is one socket: a new epoch ends the old socket and starts a new one,
  and the listener's request is pinned to the epoch (`origin::Consumer::request`
  with an owned `Epoch`, as CARLA already does), so a restarted dialer never
  inherits state. A socket ends when either side's broadcast ends or its epoch
  is replaced.
- Streams are tracks. A uni stream is a track the opener publishes in its own
  socket broadcast. A bi stream is the same track name in both. The peer
  learns of new tracks from the Lite07 Tracks stream (`TRACKS_ADD`, `REMOVE`,
  `FINISH`), which pushes them like QUIC's accept. `requested_track` was
  declined because it is pull-shaped: only the side being subscribed to can
  accept. Track names opened by the two sides must never collide (for example
  a role prefix, like QUIC's stream-id low bit); the Rust quest settles the
  scheme.
- Accept is unbounded and lazy. Announced dialers queue without limit, and a
  dialer's broadcast is requested only when the application accepts it, so an
  application that stops accepting holds no subscriptions. Backpressure on the
  announcements themselves (paced by the fastest consumer, made hard by merging
  overlapping prefixes) is a separate design.
- The convention is specified on a doc.moq.dev concept page,
  `doc/concept/socket.md`, written with the Rust implementation. It is not a
  section of the moq-lite draft or a new draft: it is an application
  convention over moq-lite.
- The demos move per-client echoes onto the socket: boy's per-viewer latency
  and CARLA's driver ack and sequence each go on that client's `.accept`
  track. State every viewer needs (the game, who is driving, who holds the
  e-stop) stays in the shared broadcast at `L`.

This epic owns removing the hand-rolled loops: after the ports, no
announce-watch-request pairing remains in `rs/moq-boy`, `js/moq-boy`, or
`demo/carla`.

## Required

- [Rust sockets](/quest/m2/socket/rust.md) - `moq_net::socket` with `Listener`, `dial`, and `Socket`, plus the concept page
- [JS sockets](/quest/m2/socket/js.md) - the same surface in `@moq/net`, interoperating with Rust
- [Python sockets](/quest/m2/socket/py.md) - moq-ffi and `py/moq-rs` bindings, interoperating with Rust
- [moq-boy on sockets](/quest/m2/socket/boy.md) - the emulator accepts viewers through a listener and echoes latency per socket
- [CARLA on sockets](/quest/m2/socket/carla.md) - trucks accept drivers through a listener and ack them per socket

## Related

- [Byte streams](/quest/m2/stream/README.md) - TCP over sockets, one bi track per connection
- [Endpoint API](/quest/m2/mesh/endpoint.md) - the mesh's `connect`/`accept`, a socket plus signing
