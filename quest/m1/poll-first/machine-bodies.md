# [M] moq-net's straight-line async bodies become machines

## Goal

Every `async fn` in moq-net whose body awaits more than one `poll_*` in a
straight line (about 23, per the poll-macro spike) becomes a
`#[kio::machine]` `poll_*` with a one-line async wrapper, so
[generated async wrappers](/quest/m1/poll-first/async-feature.md) only has to
generate wrappers for them. Behavior and public signatures are unchanged.

## Plan

Decided with the maintainer on 2026-10-28: split from
[kio::machine](/quest/m1/poll-first/machine.md) so the macro lands with its
hand-written machines first, and these conversions follow as their own PR.
Bodies with a loop or a branch around a wait stay with async-feature, which
converts them by hand.

- Re-count the candidates on current `main` and list each converted body in
  the PR, with any left as is and why.
- The existing tests are the proof; add one only where a conversion changes
  cancellation (a value that used to drop at an `.await` now lives in a named
  variant).

Public API: none. Wire: none.

## Required

- [kio::machine](/quest/m1/poll-first/machine.md) - the macro these conversions use
