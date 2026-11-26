# Redesign lite-07 subscribe and fetch

## Goal

Before cutting `moq-lite-07`, redesign lite-07 subscription, fetch, and gap accounting so that:

- Subscriptions never stall or stop making progress, and lite-07 uses zero stall-guessing timers (only spec'd media-time and arrival-age expiry for finite `Max Age`; legacy grace timers remain isolated to pre-lite-07 and `moq-transport`).
- N heterogeneous downstream subscriptions and fetches (mixing live edge, history, priorities, group orders, `Max Age`, `Min Age`, and frame offsets) deduplicate into 1 upstream `SUBSCRIBE` copy per group with zero redundant transfers and zero priority inversion.
- Gaps are proved in the data layer via `Previous Delta` on `GROUP` headers plus `RESET_STREAM_AT(header_end)` on opened Group Streams, eliminating `SUBSCRIBE_SKIP`, `FETCH`, and post-FIN `SUBSCRIBE_DROP(OLD)` contradictions on lite-07.

Pre-lite-07 versions, `moq-transport`, and post-cut warmth routing remain out of scope.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan of the exhaustive `moq-net` subscribe/fetch audit):

- **Root goal:** ✅ Redesign lite-07 subscribe/fetch wire and fix all stall and deduplication bugs before cutting `moq-lite-07`, leaving pre-lite-07, `moq-transport`, and post-cut warmth routing out of scope (rejected: bugfixes only without wire changes; drop).
- **Q1 (`FETCH` removal):** ✅ Remove `FETCH` (bidi `0x3`) on lite-07; allow `Frame == 0` in `Anchor::Position (0x2)` with exact group match (no time-containment fallback) and exclusive `End`; FIN Group Streams cleanly at `Position` End without `GROUP_END`; allow one-shot `SUBSCRIBE` + immediate subscriber `FIN` in 1 RTT (rejected: keeping stream parked open at `Position` End; keeping `FETCH` on lite-07).
- **Q2 (Data-layer gap markers):** ✅ Add `Previous Delta (i)` to the lite-07 `GROUP` header (`0` = unknown/bridged/out-of-order, `1` = first group proving `0..Group Start` empty, `d + 2` = `Previous Start = Group Start - (d + 1)` proving `(Previous Start, Group Start)` empty). Every group owns `(Previous Start, Group Start]` and `SUBSCRIBE_END(last + 1)` owns `(last, ∞)`, forming a disjoint partition of `[0, ∞)`. Keep `GROUP_END` at stream end for whole-vs-closed and presentation `End` (rejected: keeping `GROUP_END` only).
- **Q3 (`RESET_STREAM_AT` and WebTransport):** ✅ Require `RESET_STREAM_AT` on lite-07 (`reset_at(header_end, code)`) for opened Group Streams with no `SUBSCRIBE_DROP` emitted on publisher resets; accept the rare browser WebTransport race while relying on `moq-quic`'s 1-RTT header-ACK hold when `!peer_reset_stream_at`. Browser WebTransport publishers still emit `SUBSCRIBE_DROP` on stream abort, and peer `STOP_SENDING(Evicted)` before header ACK emits `SUBSCRIBE_DROP` (rejected: keeping `SUBSCRIBE_DROP` fallback whenever `!peer_reset_stream_at`; keeping `SUBSCRIBE_DROP` on every reset). Superseded 2026-11-25 (maintainer, on [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)): the header-ACK hold guarantees nothing, since a plain RESET_STREAM lets the peer discard acknowledged data, so a reset without `RESET_STREAM_AT` names its group in `SUBSCRIBE_DROP`.
- **Q4 (Per-range policy and `OLD` budget):** ✅ Move `Priority (8)` and `Group Order (8)` onto each `Range` and `Retune` in `SUBSCRIBE` / `SUBSCRIBE_UPDATE` (removing subscription-wide `Subscriber Priority` and `Group Order`), and add judged `Max Age (i)` (`ms + 1`, `0` = publisher `max_age`) onto `SUBSCRIBE_DROP` when `Error Code == OLD`, deleting `SubStream::stale_budget` (rejected: keeping single `Priority`/`Order` per `SUBSCRIBE`; moving only `Priority`).
- **Q5 (In-flight PRs):** ✅ Supersede and close [moq-dev/moq#9491](https://github.com/moq-dev/moq/pull/9491) (`quest/m2/time-addressing/per-range-start.md`) and [moq-dev/moq#9538](https://github.com/moq-dev/moq/pull/9538) (`fix/oldest-below-a-copy`) in favor of this epic. Keep `#9529`, `#9530`, `#9537`, `#9546`, `#9438`, and `#9437` (rejected: landing `#9491` and `#9538` first).
- **Q6 (Public API):** ✅ Move `priority` and `order` onto public `track::Range` across Rust, JS, and all FFI bindings (`moq-ffi`, `libmoq`, and every language wrapper), so application subscribers can request different ranges at different priorities and orders on the same `Subscription` (rejected: keeping `Subscription { priority, order, ranges }` unchanged as an internal relay detail only).
- **Q7 (`SUBSCRIBE_SKIP`):** ✅ Remove `SUBSCRIBE_SKIP` from lite-07 completely; `Previous Delta` + `SUBSCRIBE_START(None)` + `SUBSCRIBE_END` handle all silent skips, and `SUBSCRIBE_DROP` handles genuine unopened drops. First-group handling must be clean: `SUBSCRIBE_START` sets the floor so nothing below it is owed, `Previous Delta = 1` closes `0..G0` for `Start::Oldest`, and `G1` with `prev = G0` closes `(G0, G1]` for mid-pause `Start::Time` (rejected: keeping `SUBSCRIBE_SKIP`).

An exhaustive BFS state-space simulation run during planning verified the redesigned protocol:
- Model 1 (`(prev, start]` + `RESET_STREAM_AT(header_end)`): 8,256 states, 30,300 transitions, 216 quiet states; passed `ServedOnce`, `NamedOnce`, `NoContradictoryDrop`, `ZeroTimers_QuietIsSettled`, `ZeroControlSkips`, and `NoDeadlock`, while reproducing current lite-07's post-FIN `OLD` contradiction.
- Model 2 (N-to-1 dedup + per-range `(Priority, Order, MaxAge)` + judged `OLD` + unified `SUBSCRIBE` without `FETCH`): 35 policy combinations, 112 reachable relay states; passed with `max_wire_transfers_per_group == 1`, zero `stale_budget` poisoning, zero priority inversion, and zero history `OLD` drops.

Every wire change updates Rust (`rs/moq-net`), JS (`js/net`), and `drafts/draft-lcurley-moq-lite.md` in the same PR, and runs `just test interop --all`. The non-wire progress and cache-invariant fixes landed separately in moq-dev/moq#9564.
