# [S] The multi-token request cost is profiled

## Goal

The remaining multi-token gap in
`cargo bench -p moq-net --bench auth -- auth/request_tokens` is profiled,
and its cause either fixed or recorded in this quest's PR with numbers.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211),
which finishes request tokens.

Moved to m4 2026-11-12 (maintainer, /quest-audit): only the profiling remains, a benchmark nobody waits on.
The renewal half is gone: merged #7211 applies one rule to a REQUEST_UPDATE's
AUTHORIZATION TOKEN whether the subscription is paused or forwarding, refusing a
token on a request admitted without one (`TOKEN_ON_SESSION_REQUEST` in
`rs/moq-net/src/ietf/publisher.rs`).

- Single-token rounds match main; multi-token rounds are 1.5 to
  2.2x slower and grow with tokens times requests. The suspected cost is
  each announce polling its guard on the shared verdict. Profile with
  `samply` or `perf` on the bench before changing anything.

Public API: none. Wire: none.
