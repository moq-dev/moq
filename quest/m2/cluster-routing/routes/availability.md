# [M] Announcement availability

## Goal

An announcement carries availability, and letting go is a distinct offline
event, separate from the origin's END, in Rust and in `js/net`'s
announcement API (decision 3 of the epic's handoff proof). A held route
shows its claims unavailable; a let-go shows them offline; only the origin
ends them. Bindings mirror Rust.

## Plan

- Decided 2026-10-16: an announcement carries availability, and letting go
  is a distinct offline event, separate from the origin's END. Rejected:
  presence alone, which gives a reader nothing to re-request on when a held
  route recovers; let-go reported as END, as today; and availability with
  let-go kept as END, which hides whether the origin ended or the network
  lost it.
- Build on [Listing and follow](https://github.com/moq-dev/moq/pull/7678)'s
  listing and its offline report rather than adding event kinds beside it.
- JS gets the API shape here, fed by what `js/net` knows today; the
  [js/net route layer](/quest/m2/cluster-routing/routes/js.md) feeds it from the wire.
- Implemented in draft [moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375):
  `RouteEvent::Offline`, listing `Unavailable`/`Available`/`Offline` beside
  `Present`/`Absent`, and `RouteInfo::available()`, in Rust, `js/net`, FFI, the
  C ABI, and every binding. A session drop, an auth limit loss, or a narrower
  interest goes offline; a certified withdrawal ends.
- Remaining: a cleanly finished broadcast still reaches a reader through a relay
  as offline over token-scoped lite-07 sessions, so every `finish X -> Y` lane
  of `just test interop --all` fails. The router ends only a claim a floor
  certifies, and a scoped session's floor certifies nothing: its grant or limit
  (such as `finish-…/**`) does not cover the requested `""`. Certifying "the
  prefix as the grant shows it" was tried and reverted as unsound: a receiver
  need not mirror the grant, and a grant shrink drops a claim at the same
  revision (`auth::lite_07::tokens_union_and_withdrawing_one_shrinks_it`).

## Waiting

- Maintainer: choose how a token-scoped lite-07 session certifies withdrawals
  so a clean finish ENDs across a relay. It changes the draft's floor rules
  (epic decisions 10 and 12), so an agent should not pick it; done when the
  choice is recorded in this Plan. Options:
  1. (Recommended) The subscriber narrows its ANNOUNCE_REQUEST to its scope's
     single region (its limit and subscribe grant meet there) and reopens it
     when that region changes. Floors keep their exact whole-prefix meaning; no
     wire change, one SHOULD in the draft. A multi-region scope still certifies
     nothing. Best as its own prerequisite PR.
  2. Floors carry regions on the wire (lite-07-wip), reversing decision 10.
  3. Land with the finish lanes accepting offline and do 1 later; readers then
     cannot tell a finished broadcast from a lost one until it lands.

## Related

- [Routes and announces](/quest/m2/cluster-routing/routes/README.md) - the handoff contract's held and let-go states
