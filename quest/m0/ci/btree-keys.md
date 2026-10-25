# [S] Fewer BTreeMap instantiations in moq-net

## Goal

moq-net instantiates BTreeMap/BTreeSet for fewer key and value types, which make
up 21% of its library's 1.32M LLVM IR lines (about 30-40 instantiations at 7-9k
lines each; the [m2 compile-time audit](/quest/m2/audit-compile-time.md) counts
29.5% with its features). Record the library's and test target's CPU time and `cargo
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
