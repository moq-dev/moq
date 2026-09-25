# Broadcast close

## Goal

Ending a broadcast is one operation in every language: `close()`, a permanent
retraction. A closed broadcast is no longer announced to peers or discoverable
locally, serves no new tracks, and can never be announced again. Tracks already
in flight carry on and end with their own FIN or reset. Dropping the last
handle does the same, without a warning.

The finished-versus-aborted distinction goes away: `broadcast::Producer::finish`,
`abort(err)`, and `Consumer::is_finished` are deprecated, then removed on `dev`.
A broadcast has no FIN on the wire, only ANNOUNCE_START and ANNOUNCE_END, so
whether it ended cleanly never survives a hop.

## Plan

Decided with the maintainer on #4007 (the #4002 fix):

- `close(&self)` does what `finish()` does today minus the finished flag:
  retract, stop local discovery, serve no new track names, fail a later
  `announce` with `Closed`.
- Dropping a broadcast only retracts it. The "dropped without finish()" warning
  in `broadcast.rs`'s `Drop for Alive` goes, since drop is now a normal end.
- Garbage-collected bindings get an explicit `close()` rather than relying on
  finalizers.

Every change here behaves the same through a local consumer and a remote one;
only latency differs. Each child tests its scenario both in-process and over
an in-memory mock session, like `rs/moq-net/tests/finished_broadcast_mock.rs`.

`broadcast::Consumer` keeps its close signal, documented as "this broadcast
object ended", not "the path went offline": announcements say whether a path
is live, and a path can be announced again by a new object. Caches and moq-hls
use it to tell a live object from a replaced one at the same path.

Stage the `dev` removal as the child below.

## Quests

- [Remove finish](/quest/m1/broadcast-close/remove.md) - on dev, the deprecated broadcast end APIs are gone and `closed()` carries no cause

## Related

- [#4002](https://github.com/moq-dev/moq/issues/4002) - broadcast end aborted in-flight tracks; fixed by #4007, which made ending a front retract without touching its tracks
