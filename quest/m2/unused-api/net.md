# [S] moq-net publishes no item without a caller

## Goal

The moq-net crate the cut publishes has no public item that nothing outside
it calls: each one below is removed or made `pub(crate)`, and the one place
moq-lite's subscriber tells outcomes apart by matching error variants reads
a typed outcome instead.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ remove
or hide every public item the audit found without a caller, one sweep quest
per area, each item re-checked for a downstream caller first (rejected:
keeping them until someone complains; one sweep across every crate). An item
stays private until a consumer needs it, and adding it back later is
additive while removing it after the cut is a break.

Before removing an item, grep Kixel and moq.pro for a caller (the
maintainer's condition). Keep what one uses, and name the caller in the PR.

Items, each with the grep that showed no caller at `80ca9787e3` (over `rs`,
`js`, `py`, `swift`, `kt`, `dart`, `cpp`, `go`, `demo`, and `test`, outside
`rs/moq-net/`):

- `track::Position::after`, `after_group`, and `group_end`
  (`rs/moq-net/src/model/subscription.rs:610`, `:622`, `:631`): remove.
  `grep -rnF 'Position::after('`, `'after_group('`, and `'.group_end()'`
  find 0 lines outside moq-net; inside, only unit tests and `after` calling
  `after_group`. `after_group` is `start + 1`, which no longer names the
  next group now that groups are addressed by start.
- `origin::Consumer::covering` (`rs/moq-net/src/model/origin.rs:7734`): make
  `pub(crate)`. `grep -rnF '.covering()'` finds 0 lines outside moq-net; the
  lite and moq-transport sessions call it, and its doc says "a session
  serving its peer always uses this view".
- `track::Subscriber::with_range` and `track::Ordered::with_range`
  (`rs/moq-net/src/model/track.rs:10427`, `:10756`): make `pub(crate)` after
  moving their two production callers
  (`rs/moq-mux/src/container/source.rs:621`,
  `rs/moq-audio/src/decode/consumer.rs:243`) to `Subscription::ranges`, which
  is already the subscriber's read filter. The audio caller's comment gives
  a reason, that a range is aggregated across subscribers and can rewind the
  publisher for everyone, that `Range::live()` no longer has. If either
  caller cannot move, keep the method and say why in the PR. Sessions and
  resume keep using it inside the crate.
- The 17 `pub fn` predicates of `lite::Version`
  (`rs/moq-net/src/lite/version.rs`, `has_track_stream` to `has_route_cost`):
  make `pub(crate)`. They are reachable today only through the public
  `Version::Lite` variant, and no caller exists outside moq-net; the relay
  uses only the crate-level `Version::has_request_path`. Each would
  otherwise be a public name to keep or break as lite-08 moves the cut-offs.
  [The opaque `Version`](/quest/m2/net-renames.md) closes the same door from
  the other side; either order works.
- `Recovery` (`rs/moq-net/src/lite/subscriber/fills.rs:691-695`) matches
  `Error::Decode(DecodeError::Short)` and then
  `matches!(err, Error::ProtocolViolation)` to tell a reset from malformed
  bytes, against AGENTS.md's "never match on errors". The reader returns a
  typed outcome (ended, reset, malformed) that both `Recovery` and the live
  subscription read. No API change.

Not in this sweep, with their owners:

- `Subscriber::next_in_range`, `is_skipped`, and `poll_start` have one
  consumer, moq-mux's container reader, which orders groups itself.
  [Group drops](/quest/m2/group-drops.md) adds `Gap` to `track::Ordered`;
  once the container reader moves onto it these three can go private.
- `socket::is_endpoint` leaves with
  [socket endpoints](/quest/m2/sockets/remove.md).
- `origin::Producer::update_hold`, `with_proxy`, `Source`, `source()`,
  `Consumer::local()`, `origin::RouteKey`, and `Config::route_key` belong to
  [the route switch](/quest/m2/multicast/switch.md).
- `setup::Extensions` belongs to
  [the `ietf` extension](/quest/m2/ietf-records-per-track.md).

Run `just api-diff rust` after the change and list
every item it shows removed in the PR's Impact section.

Public API: breaking, removals only (`Position::after`, `after_group`,
`group_end`, `origin::Consumer::covering`, `Subscriber::with_range`,
`Ordered::with_range`, and the `lite::Version` predicates). Wire: none.

## Related

- [moq-net's node, version, handshake, and route priority](/quest/m2/net-renames.md) - makes `Version` opaque
- [A group a subscriber gives up on is always reported](/quest/m2/group-drops.md) - gives the container reader the `Gap` that frees three more methods
