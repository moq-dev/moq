# [L] Replace lite-07 SUBSCRIBE_SKIP and stall timers with Previous Delta

## Goal

On lite-07, every `GROUP` header carries `Previous Delta (i)` to prove gaps in the data layer, `SUBSCRIBE_SKIP` is removed completely, `GROUP_END` stays at stream end for whole-vs-closed and presentation `End`, and all five stall-guessing timers are removed from lite-07 (remaining isolated to pre-lite-07 and `moq-transport`). `/verify-subscribe` (`SubscribeAccounting.tla`) proves zero-timer settlement on the new wire.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan):
- ✅ Add `Previous Delta (i)` to the lite-07 `GROUP` header (`0` = unknown/bridged/out-of-order, `1` = first group proving `0..Group Start` empty, `d + 2` = `Previous Start = Group Start - (d + 1)` proving `(Previous Start, Group Start)` empty), keeping `GROUP_END` at stream end for whole-vs-closed and presentation `End` (rejected: keeping `GROUP_END` only).
- ✅ Remove `SUBSCRIBE_SKIP` from lite-07 completely; `Previous Delta` + `SUBSCRIBE_START(None)` + `SUBSCRIBE_END` handle all silent skips, and `SUBSCRIBE_DROP` handles genuine unopened drops. Handle the first group of a subscription cleanly (rejected: keeping `SUBSCRIBE_SKIP`).

Wire and accounting design (fixes audit findings W1, W5, W6, S12, S13):

- **`GROUP` header `Previous Delta (i)`:**
  - `0`: unknown predecessor (for example, bridged from lite-06 or `moq-transport`, or an out-of-order producer whose predecessor is not known).
  - `1`: first group of the track; proves `[0, Group Start)` contains no group.
  - `d + 2` (`d >= 0`): `Previous Start = Group Start - (d + 1)` (reject if `d + 1 > Group Start`); proves `(Previous Start, Group Start)` contains no group.
- **Disjoint partition of `[0, ∞)` and first-group handling:**
  - Each group `G_i` owns `(prev, G_i]` (`[0, G_0]` when `Previous Delta == 1`), and `SUBSCRIBE_END(last + 1)` owns `(last, ∞)`.
  - First group of a subscription:
    - `SUBSCRIBE_START(start)` sets the subscription floor so nothing below `start` is owed.
    - For `Start::Oldest` (`SUBSCRIBE_START(0)`), the first group `G0` carries `Previous Delta = 1`, which closes `[0, G0]` with zero control messages.
    - For a mid-pause `Start::Time(t)` where `G0.End <= t < G1.Start`, `SUBSCRIBE_START(t)` sets the floor at `t`, and `G1` carries `Previous Delta` naming `G0`, which proves `(G0, G1)` empty and closes `[t, G1]` cleanly.
    - For an empty finished track, `SUBSCRIBE_START(None)` + `SUBSCRIBE_END` closes the subscription immediately with zero groups.
  - When a publisher drops an unopened group `G_i` (`SUBSCRIBE_DROP`), the drop span covers `(prev, G_i]` (or `[start, end)` for contiguous dropped runs), so every start in `[floor, end)` is accounted for exactly once even across pauses and drops.
  - Cache-local time lookup (`AGENTS.md`): when a relay holds `G0` and `G1` linked by `Previous Delta` (`G1.prev == G0.start`), any query time in `(G0.start, G1.start)` is provably answerable from cache without an upstream round-trip.
- **Remove `SUBSCRIBE_SKIP` from lite-07 (`W1`, `W5`):**
  - Remove `SUBSCRIBE_SKIP` encoding/decoding on lite-07 in Rust (`rs/moq-net`) and JS (`js/net`), and remove it from `drafts/draft-lcurley-moq-lite.md` (keep `SUBSCRIBE_SKIP` only if needed by pre-lite-07, or verify none of pre-lite-07 uses it).
  - Delete `skipped` / `Hole::Skip` overwrite machinery on lite-07 (`W5a`, `W5b`) and fix `W1` (`SUBSCRIBE_SKIP` misclassified as `NotFound` for held fills).
  - Fix `W6b`: validate `GROUP_END` in `group.end(end)` before accounting it in `Tail`.
  - Fix `S12`: `Producer::finish_shared` clears `End::tail` properly on timed tracks and errored `SUBSCRIBE_END`.
- **Remove all five stall-guessing timers from lite-07 (`S13`):**
  - Keep wall-clock timers strictly for pre-lite-07 and `moq-transport` (plus spec'd finite `Max Age` media/arrival expiry). On lite-07, remove:
    1. `Settle` grace timer (`rs/moq-net/src/tail.rs:222-264`, `rs/moq-net/src/lite/subscriber.rs:14541-14576`)
    2. `TrackState::tail_deadline` / `End::tail` (`rs/moq-net/src/model/track.rs:4182-4238`)
    3. `Misses::deferred` / `miss_grace` (`rs/moq-net/src/model/track/miss.rs:1876-1881, 1911-1958`)
    4. `Lost` eviction TTL (`rs/moq-net/src/model/track/miss.rs:1984-2070`)
    5. `Copy::retains` / `Copy::expired` wall-clock cap (`rs/moq-net/src/model/resume.rs:1061-1155`)
  - Handle bridged `Previous Delta = 0` cleanly (when upstream is lite-06 / `moq-transport`, the copy already runs on that legacy transport's rules; a lite-07 publisher forwarding a copy with unknown predecessor sends `Previous Delta = 0` and uses explicit `SUBSCRIBE_DROP` if needed rather than silent gaps).
- **Formal verification (`/verify-subscribe`) and interop:**
  - Update `rs/moq-net/verify/subscribe/SubscribeAccounting.tla` (and its `.cfg` / README) to model `Previous Delta` + `RESET_STREAM_AT(header_end)` + no `SUBSCRIBE_SKIP`, and run TLC via `/verify-subscribe` to prove `ServedOnce`, `NamedOnce`, `NoContradictoryDrop`, `ZeroTimers_QuietIsSettled`, and deadlock freedom.
  - Update Rust (`rs/moq-net`), JS (`js/net`), and `drafts/draft-lcurley-moq-lite.md` in the same PR, and run `just test interop --all`.

Public API: none (internal `Group` predecessor metadata stays crate-private unless a public getter is needed). Wire: breaking lite-07 change (adds `Previous Delta` to `GROUP` header, removes `SUBSCRIBE_SKIP`).

## Required

- [Require RESET_STREAM_AT for lite-07 group streams](/quest/m2/lite07-subscribe/reset-at.md) - opened Group Streams deliver their GROUP header with Previous Delta before reset without emitting SUBSCRIBE_DROP
