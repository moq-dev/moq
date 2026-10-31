# [S] Fewer BTreeMap instantiations in moq-net

## Goal

moq-net instantiates BTreeMap/BTreeSet for fewer key and value types, which make
up 22.7% of its library's 1,360,948 LLVM IR lines at main 65ea670311 (the
[m2 compile-time audit](/quest/m2/audit-compile-time.md) counts 29.5% with
different features on an earlier tree). Record the library's and test target's CPU time and `cargo
llvm-lines` before and after, and keep the change only if the CPU time drops
beyond noise.

## Plan

Decided with the maintainer on 2026-10-25, as the second "thin generics" rule,
measured before adoption.

- List the instantiations by key and value type (`cargo llvm-lines` groups them).
- Options per site:
  - share one key type (for example a `u64` sequence wrapper);
  - use a `Vec` kept sorted where it stays small;
  - erase the value behind an index into a slab.
- Stay on std's map. A spike on 2026-10-25 found bbtree 0.1.5 slower on
  `TrackState`'s ascending insert, front pruning, and range lookups (2-4x on
  insert), about 9x std's IR per instantiation, and a hard dependency on Verus's
  `vstd`. Its concurrent map won only on contended reads of maps over 10k
  entries, and its scans can't give `containing()` one consistent view. Revisit
  it once it leaves 0.1.x with `vstd` optional.
- Keep each change local, and don't trade a lookup's complexity for build time on
  a hot path without a benchmark.

Public API: none.

The [build-cost audit](/.github/btree-build-audit.md) inventories 38 concrete
map key/value spellings and preserves an exact candidate patch: sharing the
expiry wall index's lossless u64 nanosecond key removes 6,485 LLVM lines. Its
2026-10-31 quiet-host repeat (12 interleaved samples per variant under
`perf stat`) shows that cut is real in instructions (-0.19% library, -0.06%
unit tests, about 30 ms of CPU per build) but not in CPU time, which stays
within noise. The candidate is not retained, so no production representation
change ships.

Each removed instantiation is worth about 0.2% of moq-net's build, so this quest
no longer earns its m0 slot. Remaining decision: delete it (recommended), or
keep it for a larger candidate that brings its own runtime benchmarks. Do not
infer a speedup from the IR count alone.
