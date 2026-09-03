# [L] Matcher

## Goal

One dependency-free Rust crate and one TypeScript package implement the v1
path-pattern grammar and exact algebra, re-exported by `moq-net`, `moq-token`,
`@moq/net`, and `@moq/token`.

## Plan

- Keep literal `Path` types and path construction unchanged. Parse patterns
  into their own canonical type and reserve `*` only when constructing or
  publishing new literal paths.
- Implement linear `matches`, `overlaps`, exact `contains`, `literal_head`, a
  total structural `specificity`, and exact set-valued `rebase`. Reduce pattern
  unions by containment; never pick one lossy residual.
- Cap patterns at the existing 32 path parts. Reject non-canonical syntax and
  bound every derived union by the finite alignments permitted by one `**`.
- Share golden Rust/TypeScript vectors for empty matches, zero-length `**`,
  ambiguous rebases, in-segment stars, containment, overlap, specificity, and
  union reduction. Add exhaustive small-alphabet tests and fuzz parse/print and
  algebraic invariants.
- Document the grammar, CAT/C4M common subset, and the literal-path rollout in
  the packages that re-export it.
