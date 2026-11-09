# [S] A room keeps a participant while one of its aliases is announced

## Goal

In `@moq/room`, `{id}/camera` and `{id}/camera.hang` (and the screen
pair) are aliases for one participant's slot. Announcing a second alias, or
withdrawing one of two, keeps the participant attached to an alias that is
still announced. The participant goes away only when its last alias is
withdrawn.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan of an external code audit of `9f51fa0966`):

- Milestone: ✅ m3 (rejected: m1), audit score 5/10.
- Fix: ✅ track the announced paths and keep a surviving alias (rejected:
  detaching only on an exact path match, which leaves an older alias
  announced with no Member once the newer one is withdrawn).

Facts at `9f51fa0966`:

- `js/room/src/room.ts:98-101` passes only identity and kind to
  `#remove` (`:116-124`), and `Remote.detach(kind)`
  (`js/room/src/remote.ts:152-156`) clears the slot whatever its path.
  Announcing the second alias also closes and replaces the first Member
  (`remote.ts:145-149`).
- The suffix is optional by design (`js/room/src/path.ts:5-6`, `:38-40`):
  hang.live omits it, and this publisher writes it (`path.ts:69-70`).
- Rust's `rs/moq-room/src/room.rs:78-80` removes by exact path, so Rust
  doesn't have this bug. Check whether Rust shows two participants for two
  aliases, and if so, make it match the JS behavior this quest settles.

Public API: none. Wire: none.
