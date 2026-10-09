# [M] Multi-CDN endpoints

## Goal

A client or publisher holds sessions to several CDNs at once, ranked by its
own preference: moq.pro as primary, Cloudflare as secondary. A subscription
goes over the most preferred link that has a route and moves to the next
one when that route goes away; a publisher pushes the same path to every CDN.
The move resumes only between lite-07 routes with the same epoch. The
secondary needs nothing from us: a moq-transport relay that speaks no ROUTE is
just a link, but its routes carry no epoch, so moving onto it is a `Restart`
([Restart](/quest/m0/broadcast-epoch/restart.md)) and the player
re-requests.

## Plan

Decided 2026-10-01 (moq-dev/moq#4694):

- Ranking: longest prefix, then the newest epoch, then the link's
  preference, then the rest of today's order (metric, hop count, rendezvous
  hash). Preference is a per-link rank the node sets (lower
  wins, equal by default), separate from the additive link cost, so metrics
  from different operators are never added together or compared; within one
  preference the metric decides as before. Babel permits any choice among
  feasible routes, so loop freedom is unchanged. The ranking is `route_order`
  in `rs/moq-net/src/model/origin.rs`; this quest adds the preference input
  and the endpoint side.
- A session that speaks no ROUTE (moq-transport, older lite) is a link whose
  announces resolve to a session-scoped origin at that link's cost.
- Identity: two CDNs never share node ids, but any route with the same path
  and epoch resumes, so failover across them needs only that. The publisher
  side is a redundant pair under one explicit epoch, from one process.
- API: the Rust client config and `js/net` accept several upstream URLs, each
  with its preference (and cost), feeding one shared origin. Pick the
  smallest shape that covers both publish and subscribe. Decided
  2026-10-08: Rust and JS only; the bindings follow when a consumer asks.

Test with mocked time: a subscriber linked to a lite-07 relay (preferred) and
a second lite-07 relay, with a publisher pushing one path and epoch to both;
losing the preferred relay resumes the subscription on the second with no
timestamp rewind, and its return moves it back. With a moq-transport relay as
the secondary, the same loss delivers `Restart` and a re-request lands on
the secondary; the move back when the preferred relay returns is the same
restart and re-request, since the secondary's routes carry no epoch.

Docs: a multi-CDN section in the routing concept page and the client
connect docs.

Public API: per-upstream preference in client config, in Rust and JS. Wire:
none.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - an epochless secondary takes over as a restart, not a resume
