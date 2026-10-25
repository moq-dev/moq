# [S] Evaluate bbtree for moq's maps

## Goal

A report on whether the bbtree crate should replace std's BTreeMap anywhere in
moq. It covers:

- runtime, with moq-net's benchmarks and a microbenchmark of each candidate's
  access pattern;
- the concurrent map (`insert(&self)`) against today's `Mutex<BTreeMap>` sites,
  such as `TrackState`'s `bounds` and `starts` in `rs/moq-net/src/model/track.rs`,
  `moq-mux` stats, and `moq-shaper`'s phases;
- build cost: LLVM IR lines per instantiation against std's, since BTreeMap is
  already the largest share of moq-net's library build;
- maturity, `unsafe` surface, Miri, wasm support, and maintenance.

It names each site to switch, or recommends staying on std.

## Plan

Requested by the maintainer on 2026-10-25. Adopt only where a benchmark shows a
win, and weigh build cost against it.

Public API: none unless a public type exposes the map.
