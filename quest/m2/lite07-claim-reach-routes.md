# [L] A lite-07 claim is shown no broader than the grants it crossed

## Goal

Every lite-07 node files and presents a claim no broader than its route can serve under the
grants that route crossed, at every hop. So a peer is never shown a claim at a path no route of
it can serve, through any number of relays. Nothing about it is local-only: the path a claim is
presented at, with its Above, carries its reach on the wire.

Today the Rust answer (`Answer::shows`, `rs/moq-net/src/lite/publisher/routed.rs`) judges the
handle's scope and the permit but never the route's reach. A producer granted `a/b` that
announces at `a` is shown to a peer asking for `a/q`, though a request there fails Unroutable.
The JS answer judges reach in-process, but no node can judge it through a relay. A relay that
takes the claim over a session granted everything files it at `a`, and shows it at `a/q` to its
own peers. The request is refused Unroutable upstream while the claim stays shown, which strands
the reader as the splice-stall did.

## Plan

Decided 2026-11-20 (maintainer, on [moq-dev/moq#9229](https://github.com/moq-dev/moq/pull/9229)):
✅ the lite-07 cut waits on this quest, and #9229 lands without it (rejected: fixing it after the
cut; holding #9229 until it is fixed). ✅ Grants become prefixes
([prefix grants](/quest/m2/prefix-grants.md)), so a route serves whole subtrees and a claim's
reach is where it is filed and presented (rejected: carrying the reach as patterns on
ANNOUNCE_START and ANNOUNCE_UPDATE, a wire field and its machinery that only wildcard grants
needed; judging reach from the routes a node knows, which a relay cannot do, so in-process and
relayed answers differ; never judging reach, which shows claims a request then fails Unroutable
on).

- A route serves the claim's subtree within its producer's grant prefixes. So:
  - The origin files a producer's claim at each of its grant's prefixes beneath the claim's own
    prefix, with Above, and as now when a grant prefix lies at or above it
    (`rs/moq-net/src/model/origin.rs`, `RouteEntry`, `sync_route`).
  - A session files a neighbour's claim where the session's grant meets the path the claim
    entered at, the same way (`lite/subscriber/routed.rs`, `admits` and `serve`).
  - An answer presents a claim no higher than where it is filed, as the draft's region rule
    already presents a claim above a region at that region. The draft says so in Claims, so a
    relay passes on what it was shown.
  - `Answer::shows` then judges only the handle's scope and the permit, and the JS answer's reach
    check becomes the same rule. Delete the reach code the filing replaces, in Rust and JS.
- One claim filed at several grant prefixes is one claim: one Announce ID per presented path,
  one Instance, and the specificity of its own prefix (`RouteEntry::specificity`). A grant
  change re-files it under #9229's SYNCED hold (`Answer::sync`).
- Exclusions (`Producer::exclude`): the relay's one caller keeps publishers out of its stats
  prefix, which the relay's own more specific claim owns, so that claim wins its paths. Test it.
  Recommended: refuse announcing a claim over an exclusion no more specific claim can own,
  rather than show a hole.
- The tests:
  - A producer granted `a/b` that announces at `a` is shown at `a/b` with Above 1. A peer
    asking for `a/q` is shown nothing, and a peer asking for `a/b/c` is shown it.
  - The same claim relayed over a session granted everything is shown the same way one and two
    hops on (the multi-hop case).
  - A neighbour granted `customer` gives a claim at the root, and it is presented onward at
    `customer` with Above 1.
  - A mount names the claim beneath its point.
  - The property test in `lite/publisher/routed/tests.rs` (`synced_follows_what_the_grant_shows`)
    announces through granted producers, so its oracle covers reach.
- Bench filing over grant prefixes per claim and claims per prefix, both swept, and the answer's
  judgment over sessions and claim churn.

Public API: none. Wire: the draft's Claims rule for where a claim is presented.

## Required

- [Prefix grants](/quest/m2/prefix-grants.md) - a grant is a set of prefixes, so a route serves
  whole subtrees
