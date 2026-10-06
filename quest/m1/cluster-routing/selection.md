# [M] Deterministic origin selection

## Goal

A broadcast under overlapping announcements routes to one origin chosen
deterministically, and a relay spreads paths over equally ranked next hops
(an edge over its region's cores) so every relay sends a given path the same
way.
Origins that announce the same concrete path are one source, so a subscriber
resumes on another when the incumbent ends or becomes unreachable.

## Plan

Most of this landed with [Wildcard](/quest/m0/wildcard/README.md) (#4403) and
#4741: `route_order` ranks the longest covering prefix first, keys its hash on
the requested path so equal-cost advertisers of one prefix split paths between
them, and a concrete path is one source with mid-group resume. Terminal
refusal landed too. What remains (cut 2026-10-06, after #4403 merged): the HRW
split test, the same-path failover test, and spreading paths over upstream
links.

Decided:

- Origin choice is per broadcast, not per announced prefix.
- A concrete path, with or without an epoch, is a source's identity: every
  origin announcing it is interchangeable, which is how a redundant pair
  works under one explicit epoch. There is no per-origin identity, for claim
  output or for paths without an epoch, and the reply names no origin
  (decided 2026-10-03: #4741 makes a path one broadcast whoever serves it).
  Failover resumes from the first frame the subscriber lacks, mid-group,
  except where a publisher starts at a group boundary, as claim workers do
  per [Wildcard](/quest/m0/wildcard/README.md)'s double-claim rule. So a
  relay never needs to tell claim output from a redundant pair.
- Every hop re-selects (decided 2026-10-01): SUBSCRIBE names no origin, since
  a pin breaks subscription aggregation.
- A refusal is terminal, and an origin sheds load by withdrawing or
  re-pricing its route instead.

Candidate mechanics:

- Upstream-link spread: at an edge, the rendezvous hash (HRW) of the
  requested path and the route's origin spreads paths over the region's
  cores and makes every edge pick the same core for a path; failover moves
  only the paths the lost core won.
- Once [Routes and announces](/quest/m1/cluster-routing/routes.md) lands, the
  candidates are the origin nodes announcing the path, ranked by longest
  prefix, then the link's preference
  ([Multi-CDN endpoints](/quest/m1/cluster-routing/multi-cdn.md)), then route
  metric to the node, then HRW. Write the ranking so that change swaps its
  inputs, not its shape.

Wire: none expected; if one is needed it goes in lite-07 (the current wip
version, where the route layer also lands) with the draft. Tests cover an HRW
split across an equal-cost pool and a same-path pair failing over mid-group
with no timestamp rewind.

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - landed the longest-prefix rule and pool spread this builds on
