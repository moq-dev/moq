# Lazy discovery

## Goal

A session fed into a local origin (`consume`) asks its peer for
announcements only under the prefixes something on that origin is watching,
opening and closing ANNOUNCE_REQUEST / SUBSCRIBE_NAMESPACE as watchers come
and go and re-opening them on reconnect, in JS and the Rust client. An app
that watches a few topics never receives announcements for the rest of what
its token allows. Subscriptions are already lazy; this makes discovery match.

Non-goals: no wire change, relay announce behavior is unchanged (a relay
watches the root, so it still discovers everything), and token claims still
cap what a peer returns.

## Plan

Decided 2026-10-10 in a `/quest-plan` interview (paper trail in the PR that
added this line). Motivated by a customer on `@moq/net` 0.4.2 who wants a
changing set of topics and today sets `discovery: false` (meant for relays
that cannot do discovery) to suppress the root ANNOUNCE_REQUEST, then opens
per-topic `session.announced(...)` streams by hand, bypassing the origin.

Shared rules for the children:

- Wire requests are a covering set: only the broadest watched prefixes go
  out, and a narrower watch rides a broader request. When a broader watch
  closes while a narrower one remains, the narrower request opens before the
  broader one closes, so nothing retracts and re-announces.
- A pattern watch requests its literal head (`room/*/chat` watches `room/`;
  `**` watches the root), the rule `interests()` uses today; the origin
  filters locally.
- When the last watcher of a prefix leaves, its request lingers for the
  connection's existing `linger` (default 2s) before closing. No new option.
- A plain `request(path)` stays blind: it opens no announce request and
  subscribes at once.
- Docs and examples that a change makes stale are updated in that change.

End to end, once all children land: the customer's flow (one connection,
topics added and removed later) works through `origin` alone, with only
per-topic requests on the wire.

## Required

- [Origin read API](/quest/m1/lazy-discovery/read-api.md) - `request(path, { announced: true })` follows restarts, `follow` and `broadcasts` are gone, and @moq/watch uses the fold
- [JS lazy discovery](/quest/m1/lazy-discovery/js.md) - js/net sessions request announcements only for watched prefixes, and the `discovery` option is gone
- [Rust lazy discovery](/quest/m1/lazy-discovery/rust.md) - the Rust client mirrors watch-driven discovery and the follow fold
