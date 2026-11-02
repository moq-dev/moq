# [M] Rust sockets

## Goal

`moq_net::socket` lets a Rust application bind a `Listener` at a path and
`accept()` the sockets dialed to it, or `dial(origin, path, id)` one itself.
Either side of a `Socket` can `open_uni`, `open_bi`, `accept_uni`, and
`accept_bi` tracks, and learns when the socket ends. `doc/concept/socket.md`
specifies the convention, so other languages can implement it from the page.

## Plan

The [epic](/quest/m2/socket/README.md) records the decisions: the
`L/.dial/<id>` and `L/.accept/<id>` layout, a dialer-chosen id with a pinned
epoch, tracks as streams discovered through the Tracks stream, and lazy,
unbounded accept.

- `Listener` watches `L/.dial/*` and queues each announced `(id, epoch)`.
  `accept()` requests the dialer's broadcast pinned to its epoch and publishes
  `L/.accept/<id>`. A new epoch for a queued or live id replaces it.
- `dial` publishes `L/.dial/<id>` and requests `L/.accept/<id>`; it resolves
  once the listener answers. Settle and record here whether a dial times out
  or waits indefinitely for a listener.
- `Socket` holds the local broadcast producer and the remote consumer, and
  exposes both for applications that use fixed track names (boy, CARLA)
  rather than open/accept.
- A bi stream returns a track producer and consumer pair; a uni stream
  returns one end. Settle and record the naming scheme that keeps the two
  sides' track names from colliding. Each opened track takes a priority from
  the caller.
- The socket ends with a typed error when either broadcast ends, its epoch
  is replaced, or the session drops.
- Write `doc/concept/socket.md` (paths, epochs, tracks as streams, ending)
  and link it from the concept index and `doc/lib/rs`.
- Tests on controlled time with no sleeps: dial and accept through a relay;
  a bi and a uni stream each way; a dialer re-announcing with a new epoch
  ends the old socket; an unaccepted dialer holds no subscription; a listener
  restart ends its sockets; a dialer for another id cannot be accepted as
  this one.
