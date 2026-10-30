# [M] moq-net's straight-line async bodies become machines

## Goal

Every `async fn` in moq-net whose body awaits more than one `poll_*` in a
straight line (about 23, per the poll-macro spike) becomes a
`#[kio::machine]` `poll_*` with a one-line async wrapper, so
[generated async wrappers](/quest/m1/poll-first/async-feature.md) only has to
generate wrappers for them. Behavior and public signatures are unchanged.

## Plan

Decided with the maintainer on 2026-10-28: split from the kio::machine quest,
which landed the macro with its hand-written machines in
[moq-dev/moq#7148](https://github.com/moq-dev/moq/pull/7148), so these
conversions follow as their own PR.
Bodies with a loop or a branch around a wait stay with async-feature, which
converts them by hand.

- Re-count the candidates on current `main` and list each converted body in
  the PR, with any left as is and why.
- The existing tests are the proof; add one only where a conversion changes
  cancellation (a value that used to drop at an `.await` now lives in a named
  variant).
- The macro has only the `impl` form: an `async fn`'s parameters become the
  struct's fields. A conditional wait is one whose poll is ready at once, and a
  wait that ends the body is `Poll::Ready(wait!(..))`; see `kio::machine`'s docs
  for the rest of the dialect.

Public API: none. Wire: none.
