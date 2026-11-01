# [L] Bound peer grant processing by design

## Goal

A peer's `AUTH_OK` costs near-linear time in its bytes on every input shape,
in Rust and JS, including a regrant compared against the previous grant.
Measure JS in a browser, not node or bun.

Re-matching live routes after a regrant is out of scope; it is
[its own quest](/quest/m2/regrant-route-recheck.md).

## Plan

[moq-dev/moq#6356](https://github.com/moq-dev/moq/pull/6356) indexed
whole-union operations in `moq-pattern` and `@moq/pattern` by segment. That
made every benchmarked shape linear, but containment between patterns with
wildcards reduces to Orthogonal Vectors, so no index closes the gap.
[moq-dev/moq#7536](https://github.com/moq-dev/moq/pull/7536) added the
`pattern-algebra` cases (Rust Criterion) and `js/pattern/bench/algebra.ts`
(Chromium through `bench/browser/micro.ts --bench pattern`) that sweep the two
worst known shapes over message size up to a whole `AUTH_OK`:

- `mask`: equal-weight `*`/`y` masks beside literal queries that fail only at
  the last segment. At 64 KiB one reduction costs about 40 ms in Rust and
  250 ms in Chromium, growing as bytes^1.7.
- `chain`: correlated partials `a^i*z^(n-i)`. At 64 KiB about 9 ms in Rust and
  24 ms in Chromium, growing as bytes^1.3.

Each `AUTH_OK` pays this more than once: Rust reduces on decode
(`Patterns::decode`) and again in `Grant::union`, then every moq-lite announce
stream's regrant runs `Permit::covers` (the same walk) against its previous
permit. JS reduces on decode (`decodePatterns`) and in `union`.

Decided 2026-11-01 (maintainer, on #7536):

- ✅ Keep received grants unreduced, in their own type. `AUTH_OK` decodes into
  a sorted, deduplicated pattern list in O(n log n). The union of received
  grants is a merge, and equality and `Permit::covers` become syntactic
  (`Permit::covers` already allows a spurious false, which only re-reads the
  origin once). Matching stays O(members) per path. `Patterns` keeps its
  reduced, set-equality promise for the grants this side issues and verifies,
  including moq-auth's `granted.covers(requested)`. Rejected: weakening
  `Patterns` everywhere (loses set equality, or splits `covers` into two
  behaviors), a joint prefix/suffix index alone (leaves `mask` at bytes^1.7),
  and refusing an `AUTH_OK` past a work budget (a runtime refusal by constant
  that changes wire behavior for valid grants).
- ✅ The regrant's route re-match is a follow-up quest, not this one.

Open, for a `/quest-plan` before implementation: the received-grant type's
name and crate, and whether `Grant` splits into issued and received types
(today one `Grant` serves `Handle::grant()` and `Credential::grant` as well as
`Request::accept`, `Issued::update`, and `Handle::authorize`, in Rust and in
`@moq/net`). Either is a breaking change to the received side's API.

Prove it with the same benches: the received-grant decode, union, and
`Permit::covers` swept over the `mask` and `chain` shapes, linear in bytes,
before and after.

## Related

- [A regrant re-checks only the routes it can change](/quest/m2/regrant-route-recheck.md) - the route fan-out after a regrant, which this leaves alone; whichever lands second adapts to the other
- [Fair share across broadcasts](/quest/m2/broadcast-fairness.md) - the other per-session CPU and bandwidth fairness concern
