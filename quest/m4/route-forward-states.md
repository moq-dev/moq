# [L] A route's forwarding and advertising decide reachability in one place

## Goal

A lite-07 node decides once whether a broadcast is ready to forward, waiting for something
named, or unavailable, and both what it advertises and where it forwards a request follow that
one decision. A request that has to wait names what it waits for, and that event wakes it, so
no reader is refused for a broadcast the node still advertises, or left waiting for something
that will never arrive. The failover model covers a claim missing for a while and a live route
coming back, and an already attached reader resumes delivery in both.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of the main → release audit): ✅ m4, after the cut
(rejected: m2, which puts an L routing refactor right before the cut; dropping it and fixing
races one replay at a time). [moq-dev/moq#9231](https://github.com/moq-dev/moq/pull/9231)
fixed the race the audit found, and
[the nightly record](/quest/m0/nightly/README.md) records the mesh replay's resolved
violations. If a replay traces a new strand to forwarding, move this quest up.

The shape today: `Node::forward` (`rs/moq-net/src/route/node.rs`) returns
`Option<Option<Via>>`, where the outer `None` waits and `Some(None)` refuses, and announce
discovery judges reachability from other facts. #9231's stranded reader came from that gap: a
route to the claimant existed before the next hop held its claim, `forward` refused for good,
and the reader had no reason to retry once the claim landed.

Recommended:

- One enum, such as `Ready(Via)`, `Waiting(Reason)`, and `Unavailable(Reason)`, computed by one
  function that both the advertised availability and `poll_forward` read. A `Waiting` reason
  names the event that wakes it, so `Unavailable` is terminal only when nothing advertised can
  change it.
- Extend `rs/moq-net/verify/failover/FailoverChain.tla` with a claim that disappears and comes
  back while its route stays up, and a route that leaves and returns live. Check that an attached
  reader resumes in both (`/verify-failover`).
- Keep [route wakes](/quest/m4/route-wakes.md)'s write-once and targeted-wake goals in view: the
  waiting reason is what lets a change wake only the requests it affects.

Public API: none expected. Wire: none.

## Related

- [Route wakes](/quest/m4/route-wakes.md) - which fronts a route change wakes
