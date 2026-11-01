# [L] Bound peer grant processing by design

## Goal

A peer's `AUTH_OK` costs near-linear time in its bytes on every input shape,
in Rust and JS, including a regrant compared against the previous grant.
Measure JS in a browser, not node or bun.

## Plan

[moq-dev/moq#6356](https://github.com/moq-dev/moq/pull/6356) indexed
whole-union operations in `moq-pattern` and `@moq/pattern` by segment. That
made every benchmarked shape linear, but containment between patterns with
wildcards reduces to Orthogonal Vectors, so no index closes the gap. The
`pattern-algebra` benchmark (Rust Criterion) and `js/pattern/bench/algebra.ts`
(Chromium through `bench/browser/micro.ts --bench pattern`) sweep the two worst
known shapes over message size up to a whole `AUTH_OK`:

- `mask`: equal-weight `*`/`y` masks beside literal queries that fail only at
  the last segment. At 64 KiB one reduction costs about 40 ms in Rust and
  250 ms in Chromium, growing as bytes^1.7.
- `chain`: correlated partials `a^i*z^(n-i)`. At 64 KiB about 9 ms in Rust and
  24 ms in Chromium, growing as bytes^1.3.

Each `AUTH_OK` pays this more than once: Rust reduces on decode and again in
`Grant::union`, then every moq-lite announce stream's regrant runs `Permit::covers`
(the same walk) against its previous permit. JS reduces on decode and in
`union`.

Candidate approaches, needing a decision:

- Keep peer grants unreduced: decode into a sorted, deduplicated list in
  O(n log n), and make `Permit::covers` syntactic (its docs already allow a
  spurious false). This changes what `Patterns` or `Grant` promise about
  equality and canonical form, so it is an API decision.
- A joint prefix/suffix index for partial segments (rectangle stabbing on
  trie traversal order). This removes the correlated-partial chain but not
  the segment-level floor.

## Related

- [Fair share across broadcasts](/quest/m2/broadcast-fairness.md) - the other per-session CPU and bandwidth fairness concern
