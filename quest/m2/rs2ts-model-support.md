# [L] rs2ts translates kio and the model subset

## Goal

rs2ts translates the Rust that kio and moq-net's model layer use, each
construct covered by its own fixtures and differential tests, and tags a
release moq can pin. The work happens in the rs2ts repo; this quest moves to
that repo's `quest/` tree once [the repo](/quest/m1/rs2ts/repo.md) exists.

## Plan

Decided 2026-10-11: kio is translated, not hand-written, with std's
`Waker`/`Context` mapped to small runtime types (generic std mappings, not
moq ones). Constructs from the 2026-10-11 inventory, with counts in non-test
code:

- Ownership: `Drop` at precise drop points (21 model impls, including
  close-on-last-drop `Alive` types), `Arc` with a refcount (134),
  `Arc::strong_count`, `Arc::new_cyclic`, `Arc::ptr_eq`, `Weak` (60), and
  RAII guards with lifetimes that wake on drop (kio `Mut<'_, T>`).
- `Mutex` and atomics (45, including `fetch_update` and a
  `wrapping_sub` delta on an unsigned atomic) as plain access.
- Closures (about 418) and iterator adapters (about 276), including
  functions returning `impl Iterator`.
- Collections: `HashMap`, `HashSet`, `BTreeMap` with `range`, `pop_first`,
  and `first_key_value`, `VecDeque`, `slab`, `arrayvec`, and `smallvec`
  (inside kio).
- 64-bit wrapping multiply (FNV-1a in `model/origin.rs:523`) and u128
  (Timescale rescale), unless the model stage removes them at the source.
- One trait object, `Arc<dyn Expiry>`: support it, or the model stage
  replaces it with an enum. Decide when this starts.

Public API: a new rs2ts release. Wire: none.

## Required

- [rs2ts repo](/quest/m1/rs2ts/repo.md) - the translator this extends
