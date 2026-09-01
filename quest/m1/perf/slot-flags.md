# [S] Un-nest group locks from track delivery

## Goal

`TrackState::poll_recv_group` and `poll_next_in_range` run under the track
mutex and, for each candidate group scanned, take the group's own mutex two
to three more times: `is_aborted` (a `state.read`), `cache_refresh` (another
`state.read` plus a clock read and an atomic stamp), and `consume`. On a
fanned-out track this is one writer and N reader threads convoying on the
track lock while it holds nested group locks. Delivery should scan
candidates without taking any group lock under the track lock.

## Plan

Mechanism, per the 2026-09 survey: the data delivery needs is already
mostly atomic-backed. `Charge::last` is an `AtomicU64` precisely so a read
guard can stamp it, and `consume`'s fast path is a `fetch_add` that only
locks when the count was zero. The nesting exists because the aborted flag
and the refresh stamp live behind the group's `kio::Lock`.

Proposal:

- Mirror the aborted state as an atomic flag on the track's `Slot` (or on a
  small shared header the slot holds), set during the abort transition,
  so the candidate scan reads a `load` instead of locking the group.
- Move `cache_refresh` out from under the track lock: stamp after the
  candidate is chosen and the track guard is dropped, or make it
  atomic-only via the existing `Charge` surface.
- Keep the documented lock order (track then group) for the paths that
  still need both; the point is the scan stops needing both.
- Loom coverage for the flag transition (`just rs loom`), since a stale
  aborted read must only ever be conservative (deliver then abort is
  acceptable; skip a live group is not).

Acceptance: `rs/moq-net/benches/track.rs` plus relay CPU at the fanout
shape via `just bench BASE` on Linux. Track-lock hold time is the mechanism
metric if a probe is cheap to add.
