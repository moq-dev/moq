# [XL] Track and group end-of-life

## Goal

In moq-net and js/net, each track and group has one outcome value: open,
ended, sealed, aborted, or dropped. Every predicate that currently re-derives
an outcome from separate flags reads that value instead, and in JS one
`closed` handle stays stable for a consumer's whole life. The Rust and JS
models change together, as the cross-package sync rule requires.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).
This is moq-net's hot path, so benchmark it as the epic says.

Bug to reproduce and fix:

- **`recoverGroup` replaces the stable handle** (`js/net/src/group.ts`).
  - The bug: `hooks.recoverGroup` resets `group.#closed = undefined`. A
    caller that read `closed` before the recovery keeps the old handle,
    latched to the error, while a new read builds a handle that can settle
    `null`. That breaks the `CombinedClosed` doc's promise of one stable
    public handle.
  - Decide in the PR whether a non-final error that a recovery continues
    past may ever reach a caller. If the current behavior turns out to be
    intended, keep it and document it in one place.

Instances to redesign:

- **Rust `TrackState`** (`rs/moq-net/src/model/track.rs`):
  - Eight fields hold its end-of-life: `end`, `end_error`, `sealed`,
    `closed`, `abort`, `resume`, `settled`, and `tail`.
  - `end` and `abort` can both be set, and which outcome wins depends on
    whether `settled` latched first.
  - `sealed || abort.is_some()` is repeated at five or more sites.
  - The start and anchor flag triples (`start_sequence`, `start_pending`,
    `start_resolved`; `received`, `anchored`, `cached_anchors`) belong to
    the same redesign.
- **Rust group** (`rs/moq-net/src/model/group.rs`):
  - `GroupState` keeps `fin` and `abort` side by side.
  - `Alive` copies the abort into three atomics (`cut`, `failed`,
    `aborted`), and readers can see the steps in between. One atomic phase
    should replace them.
- **JS track** (`js/net/src/track.ts`):
  - `TrackState` keeps `end` as a Signal and `endError` as a plain field
    that has to be set first.
  - The `Producer` wire-mode flags (`#subscribed`, `#served`,
    `#tailPending`, `#starts`).
  - `Subscriber` has two closed sources, `#closed` and `#state.closed`, and
    its guards check them inconsistently.
- **JS group** (`js/net/src/group.ts`): `#terminal` and `#verdict` are synced
  at seven sites, and one of them assigns without `??=`.
- **Resume** (`js/net/src/resume.ts` `Reader`, `rs/moq-net/src/model/resume.rs`
  `Copy`):
  - Reproduce whether a retained copy can refuse after a clean end
    (`#learnEnd` checks only `#closed`, not `#ended`). If it can, `closed`
    reports clean while reads fail.

[Group errors are cached by kind](/quest/m1/group-error-cache.md) and
[Drop set](/quest/m1/drop-set.md) edit the same track state. Coordinate with
whichever lands first rather than wait.

## Related

- [Group errors are cached by kind](/quest/m1/group-error-cache.md) - changes how group outcomes are recorded
- [Drop set](/quest/m1/drop-set.md) - rewrites the track's dropped-run state
