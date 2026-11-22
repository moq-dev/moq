# [L] An ended JS front reader still moves its open groups off a dying route

## Goal

Once a logical track ends cleanly, a JS front reader keeps moving the groups it
handed out that are still being read off a route that dies, as Rust's front does
in `Phase::Finished { routed }` (`rs/moq-net/src/model/resume.rs`). The JS
resume sweep (`js/net/src/resume.model.test.ts`) passes the cases below, and the
oracle's carve-out "Unlike Rust, an ended JS reader follows no route" is
deleted.

## Plan

Found on [moq-dev/moq#9396](https://github.com/moq-dev/moq/pull/9396). Its
received routes make this reachable more often. `MOQ_RESUME_SEED=7` already fails
on `main` the same way. Every case at 4096 cases fails with `group N ended after
1 frames where a failed route closed it`, and Rust passes each one:

- `MOQ_RESUME_SEED=7` (fails on `main` too): `write 0/0 g11 1; end 0/0; join r0
  unordered none; up 0/1; close 0/0 down; up 0/0`.
- `MOQ_RESUME_SEED=4`: `up 1/2; up 1/0; write 1/2 g11 1; join r0 ordered
  [5,12]; close 1/2; end 1/2; down 1/2`.
- `MOQ_RESUME_SEED=10`: `join r1 ordered none; ranges r1 [3,8]; up 0/1; write
  0/0 g7 1; up 0/2; down 0/0; close 0/2 down; write 0/0 next`.

In each one the track ends, then a group still being read is closed short on a
route that fails. Rust's reader stays on the route and continues the group from
the next copy. JS's `Reader.#conclude` calls `route.leave(this)`, `sync()`
returns early once `#ended`, and `Recover.#replace` ends a closed group with
`standing()` when no source serves. So the group ends where the failed copy
closed it.

Port the Rust shape: an ended reader keeps following the route while a group it
handed out is open (`Reader.out` non-empty), subscribes a copy of a new source
only to back those recoveries, and leaves once the last one closes. Port the
Rust tests for it, and update the JS tests that pin the old behavior. Then add
the three cases to the JS `regressions`.

Public API: none. Wire: none.
