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

No caller can assemble a racy two-step resolve, and no crate hand-rolls
"resolve once discovery synced".

## Plan

Decided 2026-10-22 (API audit, maintainer approved the recommendation).

The behavioral checkpoint makes `request_broadcast` own scoped discovery and
retry route churn. It reports table absence only after sync; a selected handler's
explicit refusal stays terminal, including a dropped Prepared answer. The front
records that refusal privately at its existing standing-route decision, rather
than guessing from unrelated mutations of the table. The old immediate lookup
is private to sessions. `Requesting::is_queued` and the public `Routing` task are
gone; `Requesting` is the owned task. The stats reader uses it instead of its
own retry on `is_queued`; the mux binding keeps its pinned `routed_broadcast`
wait. Rust and JS have same-epoch follow-return coverage, and every JS request
owns its discovery watch from creation. A pending plain JS request fails
`Unroutable` once that discovery syncs with no route for it, matching Rust. JS
announcement-following no longer silently uses a blind peer. The existing
`routed_broadcast` helper retains its route-change retry policy for the CLI,
gateway, mux binding, and relay HTTP consumers. Its removal belongs to the remaining rollout.

The approved public method names have not rolled out. Decided 2026-10-28
(maintainer, /quest-plan): the rollout splits into the four children below, so
each language and the callers review on their own. Rust and JS proceed in
parallel; the bindings follow Rust, and the callers follow both.

This README's own work, once every child lands: the lifecycle and churn checks
across languages, the required Nix gate, and all-language interop
(`just test interop --all`).

Public API: breaking task shape in moq-net; behavior changes in `@moq/net` and
the bindings. Final method/epoch naming remains breaking across every wrapper.
Wire: none.

## Required

- [Rust request and follow](/quest/m1/api-audit/resolve/rust.md) - Rust names the two resolutions `request` and `follow`, deletes `routed` and `routed_broadcast`, and makes the route-level `Follow` private
- [Bindings request and follow](/quest/m1/api-audit/resolve/bindings.md) - moq-ffi, the C ABI, and every wrapper mirror `request` and `follow` with an optional epoch
- [The watch player's follow prop](/quest/m1/api-audit/resolve/watch.md) - `@moq/watch`'s `Broadcast` names its resolution choice `follow` instead of `announced`
- [Callers resolve once](/quest/m1/api-audit/resolve/callers.md) - no caller hand-rolls synced resolution or decides table absence by wall clock

## Related

- [Origin fronts park](/quest/m2/origin-front-parks.md) - touches the same retry
