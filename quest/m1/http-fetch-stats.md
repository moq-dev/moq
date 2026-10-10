# [S] HTTP fetch stats

## Goal

Group bytes served over HTTP `/fetch` count in the relay's stats under the
token's tier and root, like any QUIC or WebSocket session's. An embedder can
put HTTP traffic on a tier of its own through the auth hook it already has:
the request names `Transport::Http`, so the decider picks the tier.

## Plan

Decided 2026-10-10 by maintainer priority:

- **Why.** `serve_fetch` (`rs/moq-relay/src/web.rs`) admits each request as a
  session of its own but reads through `cluster.subscriber(token)` with no
  stats context, so every byte it serves is invisible to stats while the same
  bytes over a session are counted.
- **Attach stats the way `Cluster::scope` does:**
  `self.stats.tier(token.tier).session(&token.root)` on the consumer the
  fetch reads through, held until the body ends. Each fetch is then a
  session in presence too (started on admission, ended when its lease
  closes), matching how it is admitted.
- Group reads meter through a tagged consumer, so check that both
  `fetch_group` paths (a numbered group and `latest`) land on the tagged
  track rather than an untagged one.
- `/announced` serves no media; leave it untagged unless sharing the helper
  makes tagging free.
- Test: a `/fetch` of a known group shows its bytes and one session in the
  token's root and tier, and a token on another tier keeps them apart.
- Update `doc/bin/relay/http.md` and `doc/concept/stats.md` if they say what
  HTTP counts.

Public API: none. Wire: none; the stats track's shape is unchanged.

## Related

- [Rust papercuts](/quest/m1/papercuts-rs.md) - counts `/fetch` refusals in
  `/metrics`; this counts what an admitted fetch serves
- [Bitrate claim](/quest/m2/rate-claim.md) - HTTP refuses capped tokens, so
  stats are HTTP's only per-root byte account
- [Session outcomes](/quest/m1/session-outcomes.md) - the session counters an
  HTTP fetch then joins
