# [M] A read-only origin mount

## Goal

A session's subscribe-side origin can show a subtree that lives outside its
root under a path inside it. A relay embedder grants the mount; a token cannot
ask for one. moq.pro uses it to show a project's stats feed, served fleet-wide
at `.dash/<pid>/stats`, as `<pid>/.pro/stats` inside a customer's own session,
so one `.dash` prefix route serves every project and no per-project route is
advertised.

Read-only: nothing is published through a mount, and a mount never widens
what the session can publish.

## Plan

- `origin::Consumer::mount(at, source)` returns a consumer that behaves as
  `self` everywhere except under `at`, where it resolves through `source`.
  `request_broadcast` translates the path into `source`'s root, and
  `announced()` merges both cursors, rewriting `source`'s paths under `at`.
  The mount wins over anything `self` has under `at`: the embedder chose to
  put it there.
- Everything that narrows or watches the base consumer reaches the sources
  too. `Consumer::excluding(peer)`, applied by the lite and IETF publishers
  after the relay hands over the mounted consumer, must exclude that peer
  from every source, or a route learned through the client is advertised back
  to it (split horizon). `routed_broadcast`'s retry watch must be installed
  on the source's table at the translated path, not on the base origin, or a
  request a mounted handler rejected never retries when the source's routes
  change.
- Mounts are prefix-based and scoped like any handle: `source` keeps its own
  root and patterns, so mounting an exact broadcast path exposes nothing
  beneath it.
- `moq_auth::Grant` gains `mounts`, set only by the embedder's authorizer.
  `Claims` does not carry it (`deny_unknown_fields` stays). `auth::Token`
  copies it, and `Cluster::subscriber` applies each mount to the scoped
  consumer. The publisher side ignores mounts.
- A mounted path is reachable only if the session's subscribe patterns cover
  `at`. The embedder decides whether to add the mount; the patterns still
  gate it, so the two agree.
- Lease re-checks compare mounts like root: a changed mount set closes the
  session, same as a changed root today.
- This is additive and lands on `main`. It does not reopen the overlay
  rejected in `/quest/next/wildcard/README.md`: that overlay routed publishes
  across roots. A mount is subscribe-only and has one source per path, so it
  involves no route selection, splicing, or content identity.

Tests: announce through a mount, subscribe through a mount to an unannounced
path under a dynamic prefix in `source`, a mount shadowing a local path,
patterns that exclude `at`, a publish attempt under `at` refused, a
split-horizon regression (a route learned from the client is not advertised
back through the mount), and a retry regression (a mounted handler rejects,
then a source route change makes the request resolve).

## Related

- [Origin narrowing](/quest/next/origin-narrowing.md) - the same origin/auth area; land them in sequence
- [Auth embedder](/quest/next/auth-embedder.md) - `Cluster::admit` should carry mounts too
