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
patterns that exclude `at`, and a publish attempt under `at` refused.

## Related

- [Origin narrowing](/quest/next/origin-narrowing.md) - the same origin/auth area; land them in sequence
- [Auth embedder](/quest/next/auth-embedder.md) - `Cluster::admit` should carry mounts too
