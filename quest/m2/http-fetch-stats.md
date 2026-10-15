# [S] HTTP fetch stats

## Goal

Group bytes served over HTTP `/fetch` count in the relay's stats on the
token's tier, keyed by broadcast path like a session's bytes, and each fetch
bumps a new `requests` counter under the token's tier and root instead of
counting as a session. `requests` is generic: any
request/response transport counts requests, never sessions. An embedder can
put HTTP traffic on a tier of its own through the auth hook it already has:
the request names `Transport::Http`, so the decider picks the tier.

## Plan

Decided 2026-10-10 by maintainer priority:

- **Why.** `serve_fetch` (`rs/moq-relay/src/web.rs`) reads through
  `cluster.subscriber(token)` with no stats context, so every byte it serves
  is invisible to stats while the same bytes over a session are counted.
- **Not a session.** A one-shot request has no lifetime worth a presence
  gauge, and counting it as one would inflate `sessions_started` with every
  fetch. Add `requests` beside the session counters, keyed by tier and root
  like them, and give a request a stats context that meters bytes without
  touching presence. `Handle::session` bumps presence on creation, so this
  needs its own constructor (shaped like `tier(..).request(root)`).
- Group reads meter through a tagged consumer, so check that both
  `fetch_group` paths (a numbered group and `latest`) land on the tagged
  track rather than an untagged one.
- `/announced` (`serve_announced`) serves no media, but it is an admitted
  request too: it takes the same request context and counts one `requests`.
- Test: a `/fetch` of a known group adds its bytes to that broadcast's path
  and one `requests` to the token's root, both on the token's tier; an
  `/announced` adds one `requests` and no bytes;
  `sessions_started` does not change, and a token on another tier keeps them
  apart.
- Update `doc/concept/stats.md` for the new counter, and
  `doc/bin/relay/http.md` if it says what HTTP counts.

Public API: a `requests` counter in `moq_net::stats` and a request stats
context on `stats::Handle`. Wire: the stats track gains `requests`, which an
older reader ignores and a newer one defaults to zero.

## Related

- [Bitrate claim](/quest/m2/rate-claim.md) - HTTP refuses capped tokens, so
  stats are HTTP's only byte account
- [Session outcomes](/quest/m1/session-outcomes.md) - the session counters
  `requests` sits beside
- [Grant stats label](/quest/m2/grant-label.md) - once it lands, a fetch's
  `requests` and bytes also count under the grant's label
