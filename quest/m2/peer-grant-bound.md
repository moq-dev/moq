# [L] Bound peer grant processing by design

## Goal

A peer's `AUTH_OK` costs near-linear time in its bytes on every input shape,
in Rust and JS, including a regrant compared against the previous grant.
Measure JS in a browser, not node or bun.

## Plan

[moq-dev/moq#6356](https://github.com/moq-dev/moq/pull/6356) indexed
whole-union operations in `moq-pattern` and `@moq/pattern` by segment. That
made every benchmarked shape linear, but the worst case is still about
25 ms per 64 KiB message in Rust. The shape is 1024 incomparable `*`/`a`
masks against literal queries that fail only at the last segment. Containment
between patterns with wildcards reduces to Orthogonal Vectors, so no index
closes that gap. Correlated partials (`a^i*z^(n-i)`) cost 8.6 ms, growing as
bytes^1.5 (see the
[Codex thread](https://github.com/moq-dev/moq/pull/6356#discussion_r4352967969)).

Candidate approaches, needing a fresh `/quest-plan`:

- Keep peer grants unreduced: decode into a sorted, deduplicated list in
  O(n log n), and make `Permit::covers` syntactic (its docs already allow a
  spurious false). This changes what `Patterns` or `Grant` promise about
  equality and canonical form, so it is an API decision.
- A joint prefix/suffix index for partial segments (rectangle stabbing on
  trie traversal order). This removes the correlated-partial chain but not
  the segment-level floor.

Prove it with the `pattern-algebra` benchmark extended by the mask and
partial-chain shapes, each sweeping message size.

## Related

- [Fair share across broadcasts](/quest/m2/broadcast-fairness.md) - the other per-session CPU and bandwidth fairness concern
