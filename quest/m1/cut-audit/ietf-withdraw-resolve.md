# [XS] Withdrawing a moq-transport FETCH or TRACK_STATUS ends it while it resolves

## Goal

A peer that withdraws a FETCH or TRACK_STATUS while its broadcast is still
resolving (a dynamic route, or a slow upstream on a relay) releases the
request stream, its stream credit, the origin front, and the upstream
request at once, as SUBSCRIBE does since
[moq-dev/moq#6643](https://github.com/moq-dev/moq/pull/6643) and as JS
already does. A locally closed session settles a waiting fill head instead of
dropping it unfinished.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Inherited.

- Reproduced: the `withdrawn_during` harness (`ietf/publisher.rs` about line
  11919) adapted to FETCH and TRACK_STATUS on drafts 18 and 22 left the serve
  pending 5 simulated seconds after the withdrawal in all four cases. The
  resolutions at `rs/moq-net/src/ietf/publisher.rs:2693` (standalone FETCH)
  and `:3174` (TRACK_STATUS) are wrapped only in `gate.guard`. Resolve
  through `self.until_withdrawn(&mut stream, ..)` inside the guard, like
  SUBSCRIBE at about line 1267, and extend `withdrawn_during` to both.
- Source-inferred: `State::close` (`ietf/subscriber.rs:239`) drains
  `subscribes` but not a `Fill::Ready` head, so the group producer drops
  unfinished and readers see `Error::Dropped`. `abort` and
  `remove_subscribe` already cancel it; `close` does the same.

Public API: none. Wire: none.

## Related

- [Cancelled moq-transport requests stop](/quest/m1/ietf-request-cancel.md) - the subscriber side of the same lifecycle
- [moq-transport request leases](/quest/m2/ietf-request-lease.md) - the structural fix for this class
