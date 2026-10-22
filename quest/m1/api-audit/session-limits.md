# [S] Session limits only where needed

## Goal

`moq_net::session::Limits` keeps only the bounds nothing else enforces. A
count that QUIC stream credit or the moq-transport request window already
bounds is not also a runtime refusal cap, and the documented example
compiles.

## Plan

Decided 2026-10-22 (API audit). `session.rs` `Limits` with three
`with_limits` builders (client, server, server request) caps subscriptions
and fetches, which AGENTS.md's rule against constants that only refuse at
runtime forbids where flow control already bounds the cost. The
`MIGRATE.md` example does not compile because the struct is
`#[non_exhaustive]`.

This revisits part of the per-session request caps from #4820 (the m0 DoS
review), so prove each removal first:

- For each cap, show which existing bound covers it on each version: lite
  streams consume QUIC stream credit; drafts 14 to 16 have the request
  window; later drafts have MAX_REQUEST_ID. Drop `subscriptions` and
  `fetches` where covered, with a test that the peer is still bounded.
- Keep the bounds with no other limit, such as lite announces and the
  drafts 14 to 16 request window.
- Fix the example (a constructor or `Default` plus field updates).

Public API: moq-net `Limits` fields removed. Wire: none.
