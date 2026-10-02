# [M] Multi-CDN endpoints

## Goal

A client or publisher holds sessions to several CDNs at once, ranked by its
own preference: moq.pro as primary, Cloudflare as secondary. A subscription
goes over the most preferred link that has a route and moves to the next one
when that route goes away; a publisher pushes the same epoch to every CDN so
the move splices instead of restarting. The secondary needs nothing from us:
a moq-transport relay that speaks no ROUTE is just a link.

## Plan

Decided 2026-10-01 (moq-dev/moq#4694):

- Ranking: longest prefix, then the link's preference, then the metric, then
  the rendezvous hash. Preference is a per-link rank the node sets (lower
  wins, equal by default), separate from the additive link cost, so metrics
  from different operators are never added together or compared; within one
  preference the metric decides as before. Babel permits any choice among
  feasible routes, so loop freedom is unchanged. The ranking lives in
  [Selection](/quest/m1/cluster-routing/selection.md); this quest adds the
  preference input and the endpoint side.
- A session that speaks no ROUTE (moq-transport, older lite) is a link whose
  announces resolve to a session-scoped origin at that link's cost.
- Identity: two CDNs never share node ids, so failover across them splices
  only for an epoch-qualified path; a bare path is a discontinuity. The
  publisher side is the redundant pair of
  [Selection](/quest/m1/cluster-routing/selection.md), from one process.
- API: the Rust client config and `js/net` accept several upstream URLs, each
  with its preference (and cost), feeding one shared origin. Mirror the shape
  in the bindings that expose connect config. Pick the smallest shape that
  covers both publish and subscribe.

Test with mocked time: a subscriber linked to a lite relay (preferred) and a
moq-transport relay (secondary), with a publisher pushing one epoch to both;
losing the preferred relay moves the subscription at a group boundary with
no timestamp rewind, and its return moves it back. A bare path ends and
resubscribes instead.

Docs: a multi-CDN section in the routing concept page and the client
connect docs.

Public API: per-upstream preference in client config, in Rust, JS, and the
bindings. Wire: none.

## Required

- [Selection](/quest/m1/cluster-routing/selection.md) - the ranking this adds the preference input to
