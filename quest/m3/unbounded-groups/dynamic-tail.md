# [M] Shrink live tail behind the slowest reader after frame 0 is shed

## Goal

Once a live group copy in `moq-net` (Rust and `@moq/net`) sheds frame 0, it
stops retaining up to 32 MiB of already-read frames for mid-group joins and
instead retains only what active readers still need, clamped between a 1 MiB
flow-control floor and 32 MiB (`MAX_CACHE_BYTES`), keeping the 1 MiB floor
when unwatched. Local-writer lookahead (used by `group::Producer::poll_ready`)
drops from 16 MiB to a dedicated 2 MiB limit so originating writers do not
buffer 16 MiB unread, while relay ingest keeps its existing ceiling
(`pace::AHEAD`) so paths using the full default 4 MiB QUIC stream receive
window are not throttled, and flight handover admission (`ADMIT`) stays
bounded by what the copy actually holds.

Groups that still hold frame 0 keep full 32 MiB mid-group join unchanged
(covering 4K60 4 s and 100 Mbit/s contribution GOPs). Slow readers still
never pin memory past 32 MiB or stall the writer. No wire or public API
changes.

Verified by Rust and JS unit tests covering `held == 0` preservation,
`held > 0` shrinking for 1 reader, diverging readers up to 32 MiB, unwatched
1 MiB floor, and flight handover, plus a two-relay bulk transfer measurement
showing steady-state memory drops from ~32 MiB per copy to ~1 to 2 MiB past
the initial 32 MiB.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Shrink the live tail automatically only after frame 0 is shed
  (`held > 0`), behind the slowest active reader clamped to `[1 MiB, 32 MiB]`
  (rejected: a per-track `Tail` wire field in `TRACK_INFO` as in closed
  [moq-dev/moq#9289](https://github.com/moq-dev/moq/pull/9289), which adds
  wire and API surface; unconditional 1 MiB retention while frame 0 is held,
  which drops keyframes on media GOPs larger than 1 MiB such as 1080p60 2 s
  or 4K60 2 s). Once frame 0 is gone, mid-group join is already impossible,
  so retaining frames behind the slowest active reader serves nobody.
- ✅ Give local writers (`group::Producer::poll_ready`) a dedicated 2 MiB
  lookahead limit instead of sharing `pace::AHEAD` (16 MiB), while keeping
  `pace::AHEAD` as the relay ingest ceiling so transports with a 4 MiB
  default QUIC stream receive window are not throttled (rejected: keeping
  local writers at 16 MiB, which leaves byte-stream producers buffering
  16 MiB unread; shrinking the shared `pace::AHEAD` to 2 MiB, which caps
  relay ingest below the 4 MiB stream receive window). Single frames larger
  than 2 MiB never stall a local writer, and 2 MiB covers 1 Gbit/s at 16 ms
  RTT while leaving up to 30 MiB behind the fastest reader for slower
  subscribers.
- ✅ Implement in both Rust `moq-net` and JS `@moq/net` in the same quest
  (rejected: Rust only).
- ✅ Keep the 1 MiB floor when unwatched (`readers == 0`) once `held > 0`
  (rejected: reverting to 32 MiB while unwatched, which lets a briefly
  unwatched long group balloon back to 32 MiB).
- Milestone: `m3`, under `quest/m3/unbounded-groups/`, matching its parent
  epic and additive zero-wire-change scope.

Guidance:

- In `rs/moq-net/src/model/group.rs` and `rs/moq-net/src/model/pace.rs`,
  track the slowest active cursor's position without regressing the hot
  read/write path, and have `GroupState::trim` shed behind it once
  `held > 0`, bounded to `[1 MiB, 32 MiB]` (always keeping the newest frame
  if a single frame exceeds 1 MiB, and keeping `MAX_CACHE_BYTES` as the 32 MiB
  frame and ceiling cap).
- Separate the 2 MiB local-writer lookahead constant (`group::Producer::poll_ready`)
  from `pace::AHEAD` so relay ingest (`pace::window`) keeps its existing
  ceiling (preserving the 4 MiB default QUIC stream receive window) and
  `ADMIT` stays bounded by what the target copy actually holds when handing
  over a flight.
- Mirror the dynamic tail and reader-tracking behavior in
  `js/net/src/group.ts` (`TAIL_BYTES`).

Public API: none. Wire: none.

## Related

- [Unbounded groups](/quest/m3/unbounded-groups/README.md) - parent epic owning live tails, fastest-reader pacing, and flights
