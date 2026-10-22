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
added this line). Motivated by an app on `@moq/net` 0.4.2 that watches a
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
- When the last watcher of a prefix leaves, its request lingers for a fixed
  2s before closing, matching `LINGER` in `js/net/src/connection/forward.ts`
  and the Rust child's plan. This discovery linger is separate from the
  configurable shared-connection linger; one-shot sessions have no such
  option. No new option.
- A plain JS `request(path)` stays blind: it opens no announce request and
  subscribes at once. Rust has no blind requests or answers (decided
  2026-10-20, recorded in [the Rust child](/quest/m1/lazy-discovery/rust.md)).
- Docs and examples that a change makes stale are updated in that change.

End to end, once all children land: that flow (one connection,
topics added and removed later) works through `origin` alone, with only
per-topic requests on the wire.

## Required

- [Rust lazy discovery](/quest/m1/lazy-discovery/rust.md) - the Rust client mirrors watch-driven discovery and the follow fold
