# Two broadcast resolutions

## Goal

An application resolves a broadcast in one of two ways, named the same in
Rust, JS, and every binding:

- `request(path, epoch)` resolves one publisher instance. It owns the
  discovery interest it needs, so on a watch-driven lite client it finds a
  broadcast the peer serves without the caller watching the path first, and
  it retries across route churn. It answers `Unroutable` only once discovery
  has synced with no covering route.
- `follow(path, epoch)` follows the path across publisher instances (today's
  `announced_broadcast`), pinned to `epoch` when given.

[Identity and epoch](/quest/m1/api-audit/route/identity.md) later changes the
pin from `epoch` to an `identity` in both; the two
resolutions stay as they are.

No caller can assemble a racy two-step resolve, and no crate hand-rolls
"resolve once discovery synced".

## Plan

Decided 2026-10-22 (API audit, maintainer approved the recommendation).

The behavioral checkpoint makes the Rust request own scoped discovery and
retry route churn. It reports table absence only after sync; a selected handler's
explicit refusal stays terminal, including a dropped Prepared answer. The front
records that refusal privately at its existing standing-route decision, rather
than guessing from unrelated mutations of the table. The old immediate lookup
is private to sessions. `Requesting::is_queued` and the public `Routing` task are
gone; `Requesting` is the owned task. The stats reader uses it instead of its
own retry on `is_queued`. Rust and JS have same-epoch follow-return coverage,
and every JS request owns its discovery watch from creation. A pending plain JS
request fails `Unroutable` once that discovery syncs with no route for it,
matching Rust. JS announcement-following no longer silently uses a blind peer.

Decided 2026-10-28 (maintainer, /quest-plan): the rollout splits into a child
per language and one for the callers, so each reviews on its own. Rust has
landed: `request(path, epoch)` and `follow(path, epoch)` (returning
`origin::Following`), with `routed`, `routed_broadcast`, and the route-level
`announce::Follow` gone, and its in-tree callers following the path where they
used to wait for a route. JS has landed `follow(path, {epoch})`. The bindings
have landed too: moq-ffi, the C ABI, and every wrapper name them `request` and
`follow` with an optional epoch, `follow` returning `Following`. The callers
follow both.

This README's own work, once every child lands: the lifecycle and churn checks
across languages, the required Nix gate, and all-language interop
(`just test interop --all`).

Public API: breaking task shape in moq-net; behavior changes in `@moq/net` and
the bindings. The method and epoch renames broke every wrapper.
Wire: none.

## Required

- [The player takes any read-side origin](/quest/m1/api-audit/resolve/watch-origin.md) - `@moq/watch`'s `Player` and `Broadcast` accept a read-only `Origin.Consumer` as well as a `Table`
- [JS follow returns a Following](/quest/m1/api-audit/resolve/js-following.md) - `follow` and `request` get their own JS types, matching Rust and the bindings
- [JS follow takes FollowOptions](/quest/m1/api-audit/resolve/follow-options.md) - `follow` and `request` get their own JS options types

## Related

- [Origin fronts park](/quest/m2/origin-front-parks.md) - touches the same retry
