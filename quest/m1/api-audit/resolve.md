# [L] Two broadcast resolutions

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
gone; `Requesting` is the owned task. The mux sibling resolver and stats reader
use it instead of their own sync/retry helpers. A binding retains its first
verdict and releases the completed request, so an errored reference never
silently retargets to a later publisher. Rust and JS have same-epoch
follow-return coverage, and every JS request owns its discovery watch from
creation. JS announcement-following no longer silently uses a blind peer.
The existing `routed_broadcast` helper privately retains its route-change retry
policy for active CLI, gateway, and relay HTTP consumers. Its removal belongs
to the remaining rollout, after their replacement is settled.

The approved public method names have not rolled out. What remains:

- Rust: name the owned operation `request(path, epoch)` and today's
  `announced_broadcast` operation `follow(path, epoch)`. Keep the immediate
  table lookup private. Delete `routed` and `routed_broadcast` and fix their
  callers. Preserve terminal selected-handler refusals, private deferred
  validity checks, and instance identity across route churn.
- Move the active route-level `Follow` consumers before making it private.
  `moq play`, `moqsrc`, and the mux TS follower still consume its Start, End,
  and Restart events. Their replacement belongs to the existing
  `Source::follow` work below, whose shape is explicitly open. Do not add a
  new public route API or compatibility shim to make the privatization pass.
- JS: expose `request(path, {epoch})` and `follow(path, {epoch})` instead of the
  `announced` option and migrate actual callers. Make a one-instance request's
  synced-absence verdict terminal, matching Rust and the bindings, while a
  follower remains open. The current JS handle exposes it through `unroutable`.
  The checkpoint covers pending
  one-instance retry through a route gap without retargeting an already resolved
  instance. Retain synchronous request peeks and path-local wakeups.
  `Producer` pass-throughs stay only where `Table` needs them.
- Bindings: mirror `request` and `follow`, including the optional epoch, in
  the FFI and every actual wrapper/consumer. Regenerate from source, update
  package migrations, and run all-language interop. Existing binding names
  currently inherit the owned-discovery behavior, not the final naming.
- Callers: remove the racy two-step lookup and hidden error in moq-wasm,
  route waits in moq-cli subscribe/report, and the HLS hand-rolled discovery
  and `RESOLVE_TIMEOUT`. Review SRT, transcode, benchmark connections, relay
  HTTP fetch deadlines, the e2ee epoch advice, and the demo's announce-derived
  epoch pin. No caller should hand-roll synced resolution or rely on a wall
  clock to decide table absence.

Tests cover an announced but unwatched path over lite-05/06/07-wip, synced
absence, discovery retention and release, pinned follow return, no follower
fallback on a blind peer, and unchanged explicit refusal. Finish the API and
caller rollout with the same lifecycle and churn checks, then the required
Nix gate and all-language interop.

Public API: breaking task shape in moq-net; behavior changes in `@moq/net` and
the bindings. Final method/epoch naming remains breaking across every wrapper.
Wire: none.

## Required

- [Reference announcements](/quest/m0/broadcast-epoch/reference-announcements.md) - settles `Source::follow`, the existing owner of the active route-level consumers' replacement

## Related

- [Readers follow restarts](/quest/m1/sink-restart-follow.md) - moves CLI and GStreamer readers to the shared follow operation after reference following
- [Origin fronts park](/quest/m2/origin-front-parks.md) - touches the same retry
