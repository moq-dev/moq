# [S] Collapse a window's layered runs to one shared set

## Goal

`rs/moq-net`'s subscription window holds its groups as plain shared runs,
without the moving layer that time ends once walked, and every path that
diffed or searched that layer is gone, with no change in what readers get.

## Plan

Since [the model](/quest/m2/time-addressing/model.md), a time end settles at
once on keys, so `Window::rebuild` always builds `Layered` with an empty
layer. What still assumes one:

- `model/subscription/runs.rs`: `Layered`, its `gained` and `minus`, and the
  property tests over a moving layer.
- `model/track.rs`: `find_servable` leapfrogging the layer after the fixed
  runs, `Admitted::gained`, and `Servable::layered` in its property test.
- `model/track/miss.rs`: the misses view that judges only what a moved layer
  covered, and `shared_layer` in its property test.

Replace `Layered` with `Arc<Runs>`, comparing windows by pointer where they
compared fixed runs. A window that moves now rebuilds, so the incremental
paths become a full recompute of what is owed; keep the property tests that
check owed groups and servable finds against a scan.

Public API: none. Wire: none.

## Required

- [Rust groups by start](/quest/m2/time-addressing/model.md) - deletes the time-end walks that fed the layer
