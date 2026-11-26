# [L] Remove FETCH from lite-07 and unify into SUBSCRIBE

## Goal

`FETCH` (bidi `0x3`) is removed from lite-07 (retained only for pre-lite-07 and `moq-transport`). `Anchor::Position (0x2)` allows `Frame == 0` with exact group matching and exclusive `End`, Group Streams FIN cleanly at a `Position` End without `GROUP_END`, one-shot `SUBSCRIBE` supports immediate subscriber `FIN` in 1 RTT, and all fetch callers (`fetch_group`, `fetch_range`, HTTP `/fetch`, head fills, shed-frame flights, mid-group failover, and single-group misses) route through `SUBSCRIBE` so overlapping fetches and live subscriptions deduplicate into 1 wire copy.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan): ✅ Remove `FETCH` (bidi `0x3`) on lite-07; allow `Frame == 0` in `Anchor::Position (0x2)` with exact group match (no time-containment fallback) and exclusive `End`; FIN Group Streams cleanly at `Position` End without `GROUP_END`; allow one-shot `SUBSCRIBE` + immediate subscriber `FIN` in 1 RTT (rejected: keeping stream parked open at `Position` End; keeping `FETCH` on lite-07).

Wire and deduplication changes (fixes audit findings D1, D2, D5, W5c, W6a):

- **`Anchor::Position (0x2)` semantics:**
  - Allow `Frame == 0` in `Anchor::Position(Group Start, Frame)`.
  - Exact group match only: `Position(G, f)` matches only a group with `Group Start == G` (never falls back to an earlier group whose span contains `G`). Keep `Anchor::Time(Timestamp)` exclusively for time-containment lookup.
  - Exclusive `End`: `[Position(G, 0), Position(G + 1, 0))` requests whole group `G` (works for `G == 0` without underflow); `[Position(G, f1), Position(G, f2))` requests exact frame slice `[f1, f2)`.
  - Reaching a `Position` End on a Group Stream sends clean `FIN` (without `GROUP_END`, since it is a slice, not the group's whole end) instead of parking the stream open (fixes W5c, where a parked stream later expired and emitted `SUBSCRIBE_DROP(OLD)`).
- **One-shot `SUBSCRIBE` + immediate subscriber `FIN`:**
  - A subscriber may send `SUBSCRIBE` and immediately `FIN` its write half of the Subscribe Stream (1 RTT total, zero follow-up `SUBSCRIBE_UPDATE`s).
  - The publisher treats subscriber `FIN` on the Subscribe Stream as "no further updates; deliver all currently requested ranges and close the Subscribe Stream cleanly once every range is accounted for" (unlike a stream reset/cancel, which aborts delivery).
- **Remove `FETCH` (`0x3`) on lite-07 (`D1`, `W6a`):**
  - Remove `FETCH` stream type `0x3` from lite-07 in Rust (`rs/moq-net`), JS (`js/net`), and `drafts/draft-lcurley-moq-lite.md`. Keep `FETCH` only for lite-01..06 and `moq-transport`.
  - Every lite-07 fetch now runs through `TrackRun` / `GroupServe` / `SendGroup` scheduling (eliminating D1 where ungrouped bidi `FetchServe` strictly preempted every live audio/video `SUBSCRIBE` stream, skipped `ietf::flate::egress*`, skipped yield budgets, and skipped `poll_settle`) and through `GroupRecv` (eliminating W6a where `FetchServeRun` omitted group-end gap handling).
- **N-to-1 fetch + live `SUBSCRIBE` deduplication (`D2`, `D5`):**
  - Route `fetch_group`, `fetch_range`, HTTP `/fetch`, headless group head fills, shed-frame flights, mid-group failover, and single-group misses through `SUBSCRIBE` ranges on lite-07.
  - Remove the `spans_groups` gate that forced 1-group fills, frame-offset fills (even 100-group spans), and pre-`TRACK_INFO` fills onto sequential 1-by-1 wire `FETCH`es (D5).
  - When `fetch_group(N)` or `fetch_range` arrives at a relay whose active upstream `SUBSCRIBE` already covers group `N` (and has not yet delivered-and-evicted or dropped `N`), union the request into that active upstream `SUBSCRIBE` (and make in-flight slots visible to `admit_group` / `poll_fetch_cached`) so at most **1 wire copy** of group `N` transfers (fixing D2).
  - When a group was already delivered-and-evicted or dropped on the main upstream `SUBSCRIBE`, issue a fresh one-shot bounded `SUBSCRIBE` (with immediate subscriber `FIN`) instead of wire `FETCH` or a separate `Recovery` protocol.
- **Cross-package sync and verification:**
  - Update Rust (`rs/moq-net`), JS (`js/net`), and `drafts/draft-lcurley-moq-lite.md` in the same PR.
  - Add unit/integration tests proving: `fetch_group(5)` + live `SUBSCRIBE` at playhead 5 transfers exactly 1 wire copy of group 5; low-priority fetch does not preempt high-priority live `SUBSCRIBE`; frame-slice `[Position(G, f1), Position(G, f2))` FINs cleanly without `GROUP_END` or spurious `OLD` drops; one-shot `SUBSCRIBE` + immediate `FIN` completes in 1 RTT.
  - Run `just test interop --all`.

Public API: none (public `fetch_group` / `fetch_range` methods stay, backed by `SUBSCRIBE` on lite-07). Wire: breaking lite-07 change (removes `FETCH` `0x3`, changes `Anchor::Position` rules and one-shot `SUBSCRIBE` FIN semantics).

## Required

- [Remove lite-07 stall timers: GROUP_END and SUBSCRIBE_SKIP name every empty start](/quest/m2/lite07-subscribe/previous-delta.md) - bounded SUBSCRIBE relies on its gap accounting and clean group stream termination
