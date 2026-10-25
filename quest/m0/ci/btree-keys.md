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
- [bbtree](/quest/m2/bbtree.md) is evaluated separately; if it replaces std's map,
  count its instantiations instead.
- Keep each change local, and don't trade a lookup's complexity for build time on
  a hot path without a benchmark.

Public API: none.
