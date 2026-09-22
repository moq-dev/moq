# [XS] Rust mutate beside the guard

## Goal

The simple in-place edit reads the same in both languages: JS
`Producer.mutate(callback)` edits a clone and publishes on return, and Rust
gains `mutate(|value| ...)` on `moq_json::snapshot::Producer` with the same
rules, while `modify()` keeps the guard for callers who need to hold the lock
across several edits.

## Plan

Implement `mutate` on top of `modify`: open the guard, run the closure on the
value, and commit, returning the publish error the guard would otherwise
raise on drop. Mirror it on `moq_mux::catalog::Producer` if the JS catalog
producer exposes the same call; otherwise leave the catalog guard alone.
Document the pairing in one line on each method and in the upgrade page.

Public API: one additive method per producer. Wire: none.
