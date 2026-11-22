# [XS] Narrow Room.remotes to ReadonlyMap in @moq/room

## Goal

`Room.remotes` in `@moq/room` is typed as `Getter<ReadonlyMap<...>>` so
consumers cannot mutate the room's internal map in place behind the signal's
back.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ narrow `Room.remotes` in `m2` before `0.22.0` freezes
the `@moq/room` TypeScript surface (rejected: skipping the API freeze
cleanups).

- Change `Room.remotes` at `js/room/src/room.ts:49,77-79` from
  `Getter<Map<...>>` to `Getter<ReadonlyMap<...>>`.
- Update `js/room/api/index.api.md:187` (and check any workspace callers in
  `js/` that read `room.remotes`).

Public API: breaking in `@moq/room` (narrows `Map` to `ReadonlyMap`). Wire:
none. Run `just js check` and `js/room` tests.
