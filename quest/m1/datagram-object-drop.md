# [S] Datagrams past object 0 never pose as object 0

## Goal

A moq-transport datagram group with several objects reaches every surface
that keeps Object IDs intact, and every surface that cannot carry an Object ID
delivers only object 0. Today a moq-lite hop without the `ietf` extension
forwards every object as a plain datagram at the same sequence, and the
receiver keeps whichever arrives first. If object 0 is lost or reordered,
object 1 is delivered as the group's only datagram, which is silent
substitution, not loss. The plain Rust datagram reader also hands out every
object's payload at one sequence with no way to tell them apart.

Out of scope: exposing an Object ID on the public `Datagram` in Rust, JS, or
the bindings; refusing the track; a dropped-datagram stat; multi-object
datagram support in JS.

## Plan

Decided 2026-10-26 (maintainer, importing
[moq-dev/moq#6885](https://github.com/moq-dev/moq/issues/6885)):

- Keep the passthrough from
  [moq-dev/moq#5494](https://github.com/moq-dev/moq/pull/5494). Rust relays
  already carry Object IDs between moq-transport peers and across a lite-07
  hop with the `ietf` extension. The report was against release 0.17, whose
  IETF ingress still dropped every object past 0. Rejected: refusing at
  ingress everywhere, which deletes that passthrough.
- Drop, don't refuse. A hop that cannot carry the Object ID sends only
  object 0, matching how a hop drops an oversized datagram or a status object
  it cannot carry. The rest of the track keeps flowing. Rejected: aborting the
  track with `Unsupported`, which would also cost the lite viewer object 0.
- Surfaces:
  - `lite::Publisher::serve_datagram` (`rs/moq-net/src/lite/publisher.rs`),
    when `ietf` is off: skip a datagram whose `wire.object_id` is past 0, as
    it already skips a status.
  - `track::Subscriber::poll_recv_datagram` (`rs/moq-net/src/model/track.rs`):
    skip a stored datagram past object 0, as it already skips a status.
  - JS's IETF subscriber already drops past object 0 and needs no change.
- Debug log like the other per-hop drops; no new stat.
- Fix the docs inline: `doc/concept/standard.md` (the objects and datagrams
  bullet says plain readers and lite peers see "normal datagram payloads")
  and the `model/datagram.rs` module doc. The cursor's private
  `poll_recv_datagram` comment says ranges match "by Object ID", which the
  code does not do; make it say what the code does.
- Test both surfaces with a group carrying objects 0 to 2, including object
  0 arriving last and object 0 never arriving.

## Closes

- [#6885](https://github.com/moq-dev/moq/issues/6885) - moq-relay silently drops datagram objects after object 0 in a group

## Related

- [moq-transport 18 and 22 only](/quest/m1/ietf-drop/README.md) - the reporter's draft 16 is among the drafts that quest deletes
