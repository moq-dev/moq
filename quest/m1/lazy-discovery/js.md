# [L] JS lazy discovery

## Goal

A js/net session with a `consume` origin opens announce requests only for
prefixes the origin's readers are watching, per the
[line's shared rules](/quest/m1/lazy-discovery/README.md), and re-opens them
on every reconnect. The `discovery` connect option and
`Established.discovery` are deleted. @moq/watch watches only the broadcasts
its catalog actually references, never the root on their behalf.

## Plan

Decided 2026-10-10 in the [lazy discovery](/quest/m1/lazy-discovery/README.md)
interview.

Facts (2026-10-10, `origin/main` e5446b036):

- `forwardAnnounced` opens one `conn.announced(subtree(prefix))` per
  `originWire.interests()` once, at connect (`js/net/src/connection/forward.ts:51`);
  `interests()` is the scope's heads, so an unscoped origin requests the root.
- `discovery: false` skips that and marks the session non-discovering, which
  makes `request(path, { announced: true })` subscribe blind. The flag is
  threaded through `connect.ts`, `accept.ts`, `pool.ts`, `reload.ts`, and both
  protocol connections. `Established.discovery`'s doc says `announced()` never
  yields, but nothing enforces it.
- @moq/watch watches the root with `origin.announced(Path.Pattern.all(),
  { hidden: true })` whenever a catalog has relative references
  (`js/watch/src/broadcast.ts:172`).

Decisions:

- What counts as a watch: `announced(scope)` and
  `request(path, { announced: true })` (after the
  [read API](/quest/m1/lazy-discovery/read-api.md) fold). Each holds its
  watch until closed.
- The origin exposes the live watched-prefix set to its sessions through the
  `wire.ts` seam; each session maintains the covering set against it.
- Delete the `discovery` option. A relay that refuses or resets an announce
  request already downgrades the session to blind answers (the fallback in
  `forward.ts`); that becomes the only path. The cost is one wasted round
  trip on the first watch against a relay without discovery, replacing
  today's warning-only early return (`forward.ts:45`). The downgrade covers
  the whole session, so later watches on it go blind without retrying.
- A reconnect re-opens only prefixes that still have watchers; a request
  lingering with none is dropped, not re-opened.
- @moq/watch resolves each relative reference first
  (`Path.tryResolve("a/b/main.hang", "../x")` is `a/x`) and watches each
  resolved target, letting the covering set merge them. No parent or
  common-ancestor heuristic, which would miss `a/x` or widen to the root.

Tests (mock time for the linger): no watcher means no announce request;
watch, unwatch, and linger expiry open and close the request; nested watches
share one request and hand over without a retraction; a reconnect re-opens
every live watch and skips a lingering one with no watchers; a pattern
watches its literal head; a refused announce request downgrades the session
once and later watches open nothing; @moq/watch with `../x` from
`a/b/main.hang` watches `a/x` and opens no root request. Benchmark the covering-set update
swept over watcher count and session count.

Public API: `discovery` removed from connect, accept, and `Connection` props,
and `Established.discovery` removed; a `consume` origin no longer fills until
something watches. Note the removal and the fill change in the PR's
migration notes and `doc/lib/js/net.md`. Wire: none.

## Required

- [Origin read API](/quest/m1/lazy-discovery/read-api.md) - the watch set is defined over the folded API
