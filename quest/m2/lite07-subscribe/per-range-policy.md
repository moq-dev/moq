# [L] Per-range priority, order, and judged OLD drops on lite-07

## Goal

On lite-07 and in the public `track::Range` API across Rust, JS, and all FFI bindings, `Priority` and `Group Order` live on each `Range` (and `Retune`) rather than once per `SUBSCRIBE`. `SUBSCRIBE_DROP(OLD)` carries the judged `Max Age` (`ms + 1`, `0` = publisher `max_age`), deleting `SubStream::stale_budget`. `TrackRun` answers every range independently without subscription-wide `Start::Unresolved`, `withheld`, `placed_floor`, or `taken_back` state, and a model proves N-to-1 heterogeneous deduplication.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan):
- ✅ Move `Priority (8)` and `Group Order (8)` onto each `Range` and `Retune` in lite-07 `SUBSCRIBE` / `SUBSCRIBE_UPDATE` (removing subscription-wide `Subscriber Priority` and `Group Order`), and add judged `Max Age (i)` (`ms + 1`, `0` = publisher `max_age`) onto `SUBSCRIBE_DROP` when `Error Code == OLD`, deleting `SubStream::stale_budget` (rejected: keeping single `Priority`/`Order` per `SUBSCRIBE`; moving only `Priority`).
- ✅ Move `priority` and `order` onto public `track::Range` across Rust, JS, and all FFI bindings (`moq-ffi`, `libmoq`, `py/`, `swift/`, `kt/`, `go/`, `dart/`, `cpp/`), so an application subscriber can fetch old content at a different priority/order on the same `Subscription` (rejected: keeping `Subscription { priority, order, ranges }` unchanged as an internal relay detail only).
- ✅ Supersedes `quest/m2/time-addressing/per-range-start.md` ([moq-dev/moq#9491](https://github.com/moq-dev/moq/pull/9491)) and [moq-dev/moq#9538](https://github.com/moq-dev/moq/pull/9538).

Wire, API, and state-machine changes (fixes audit findings S3, S8, S9, S10, S11, D3, D4):

- **Per-range `Priority` and `Group Order` on wire and public API (`D4`):**
  - Remove subscription-wide `Subscriber Priority (8)` and `Group Order (8)` from lite-07 `SUBSCRIBE` and `SUBSCRIBE_UPDATE` (`Retune` carries per-range `Priority` and `Group Order`). Keep the legacy subscription-wide fields on pre-lite-07 and `moq-transport` (derived from the merged demand).
  - Move `priority` and `order` from `track::Subscription` onto `track::Range` in Rust (`rs/moq-net`), JS (`js/net`), and every FFI binding (`rs/moq-ffi`, `rs/libmoq`, `py/`, `swift/`, `kt/`, `go/`, `dart/`, `cpp/`). Update all in-tree callers (`hang`, `moq-relay`, `moq-mux`, `moq-cli`, `js/hang`, examples, tests) and include the migration note in the PR's `## Impact` section for `MIGRATE.md`.
  - **Publisher scheduling for a group `G`:** Compute the set of active ranges covering `G` whose `Max Age` has not expired `G`. If empty, `G` is stale (`OLD`, judged against the widest `Max Age` of the covering live ranges). Otherwise, transmit `G` once at the highest `Priority` among wanting ranges, using the `Group Order` of that top priority (`Descending` breaking ties). When a group ages past a high-priority live range's `Max Age` while a low-priority history range still covers it, its effective priority and order automatically drop to the history range's (preventing priority inversion and history shedding under stream-credit pressure).
  - **Demand unioning (`demand/ranges.rs`):** Never collapse a high-priority or finite-`Max Age` live range into a lower-priority `Range::ALL`, so live priority and live-edge `Max Age` stay visible upstream without extra `join_probes`.
- **Judged `Max Age` on `SUBSCRIBE_DROP(OLD)` (`D3`):**
  - On lite-07, when `SUBSCRIBE_DROP` has `Error Code == OLD`, append `Max Age (i)` encoded as `ms + 1` (or `0` when judged against publisher `max_age`, which settles every reader).
  - Delete `SubStream::stale_budget` (`rs/moq-net/src/lite/subscriber.rs:11298-11303, 11650`) and its JS counterpart completely. Relays record the exact judged `Max Age` from the wire so a transient `Start::LATEST` (`max=0`) viewer never poisons wider readers on the same upstream subscription.
- **Pure per-range `TrackRun` state (`S3`, `S8`, `S9`, `S10`, `S11`):**
  - Replace `TrackRun`'s legacy subscription-wide `Start::Unresolved`, `Start::Resolved(from)`, `withheld`, `placed_floor`, and `taken_back` layers with per-range `RangeState::Waiting | Started` on every track (origin and relay copy alike) in Rust (`rs/moq-net/src/lite/publisher.rs`) and JS (`js/net/src/lite/publisher.ts`).
  - Fixes `S3` (`placed_floor` clipping unresolved `Start::Time` below `self.floor`), `S8` (`Start::Unresolved` starving `poll_recv_next` and expiry), `S9` (`asked.live` hanging when `Edge::Unknown` on finished track), `S10` (empty finished track `final_key == 0` leaving `Start::Unresolved` forever), and `S11` (`withheld` drops stranded permanently).
- **Formal verification and cross-package sync:**
  - Add a TLA+ or exhaustive property model verifying N-to-1 heterogeneous deduplication across overlapping live/history ranges with distinct `(Priority, Order, MaxAge)` and judged `OLD` drops (extending `rs/moq-net/verify/subscribe/`).
  - Update Rust (`rs/moq-net`, `rs/moq-ffi`, `rs/libmoq`, all bindings), JS (`js/net`, `js/hang`), and `drafts/draft-lcurley-moq-lite.md` in the same PR.
  - Run `just test interop --all`.
- **History given up before the first start (from closed [moq-dev/moq#9491](https://github.com/moq-dev/moq/pull/9491)):** the per-range state must not forget stale runs a relay gives up while a range still waits for its start. Branch [`quest-iterate/9491-narrow`](https://github.com/moq-dev/moq/tree/quest-iterate/9491-narrow) (81713d43) forgot them and fails `widened_max_age::a_tightened_start_leaves_history_fillable_by_an_intermediate_reader` with relay=true:
  - A reader narrows its live join's max while the relay's join still waits for its start, and the relay gives up groups 2..5 as stale in that window.
  - The join is then answered at group 2 and owes 2..5, which nothing ever names. A wider reader joining later adds a range to that subscription, never told the groups were dropped as too old, and never receives 4 and 5 (still missing after 1000 simulated seconds against a 1s grace), so the history is lost for good.
  - Recommendation from that work (untested): keep the stale runs given up before a range's first `SUBSCRIBE_START`, and forget them only after it, which keeps memory bounded as `main` does. Rejected there: holding stale runs while any range waits.
  - Keep that test passing with relay=true.

Public API: breaking change (moves `priority` and `order` from `Subscription` onto `track::Range` across Rust, JS, and all FFI bindings; PR title uses `!` and `## Impact` includes the migration note). Wire: breaking lite-07 change (per-range `Priority`/`Group Order`, `Max Age` field on `SUBSCRIBE_DROP(OLD)`).
