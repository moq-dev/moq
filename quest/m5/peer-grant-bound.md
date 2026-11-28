# [S] Bound peer grant processing by design

## Goal

A peer's `AUTH_OK` costs near-linear time in its bytes on every input shape,
in Rust and JS, including a regrant compared against the previous grant.
Measure JS in a browser, not node or bun.

Re-matching live routes after a regrant is out of scope; it is
[its own quest](/quest/m4/regrant-route-recheck.md).

## Plan

Since [moq-dev/moq#9251](https://github.com/moq-dev/moq/pull/9251), `AUTH_OK`
carries prefixes, not patterns. Containment between patterns with wildcards
reduces to Orthogonal Vectors, which made the `mask` and `chain` shapes cost
bytes^1.7 and bytes^1.3, but no peer input carries a wildcard any more. That
also retires the 2026-11-01 decision to keep received grants unreduced in
their own type: `Prefixes` reduces in one sort and one sweep in `moq-net`
and `@moq/net`, so issued and received grants keep one type.

[moq-dev/moq#9251](https://github.com/moq-dev/moq/pull/9251) added prefix rows
(`collect`, `covers`, `intersect`): distinct members two segments beneath
their parents, filling 4 KiB, 16 KiB, and a whole 64 KiB `AUTH_OK`. One local
Rust run at 64 KiB took 0.27 ms to collect, 2.8 ms for `covers`, and 6.2 ms to
intersect. From 4 KiB to 64 KiB, `collect` grew 16x and the other two about
22x, so n log n. The Rust rows now live in `rs/moq-net/benches/prefixes.rs`;
the wildcard `mask` and `chain` rows went with `moq-pattern` and `@moq/pattern`
([moq-dev/moq#9800](https://github.com/moq-dev/moq/pull/9800)), which also
dropped the JS rows.

Remaining:

- Add the prefix rows to a JS browser bench (`bench/browser/micro.ts`), run
  it in Chromium, and record them.
- Sweep the shapes that stress prefixes instead: deep members (32 segments)
  and long segments. `matches` binary-searches each ancestor of a path, up to
  33 searches per member. If a shape grows faster than n log n, make it one
  predecessor search: in a reduced set, the only member that can cover a path
  is the greatest member not after it.

## Related

- [A regrant re-checks only the routes it can change](/quest/m4/regrant-route-recheck.md) - the route fan-out after a regrant, which this leaves alone
- [Fair share across broadcasts](/quest/m5/broadcast-fairness.md) - the other per-session CPU and bandwidth fairness concern
