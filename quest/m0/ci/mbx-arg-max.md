# [S] mbx launches Cargo with any argument length

## Goal

mbx launches Cargo however long its argument list or environment is, and
`RecipeTest.test_unexecuted_bench_is_not_compiled` (`sh/rs/select.test.py`)
passes in `just ci`.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning). A `just ci` run during
[moq-dev/moq#7567](https://github.com/moq-dev/moq/pull/7567) failed in that
test, because mbx couldn't launch Cargo: "Argument list too long" (`E2BIG`).

- First find which part goes over the limit: the argv, or the environment
  the test passes (`os.environ` plus its overrides).
- Then fix it at the cause, for example by passing long lists through a file
  or by trimming what mbx forwards, rather than skipping the test.
- If the limit is hit inside mbx itself rather than in this repository's
  wrapper, the fix belongs upstream, and this quest pins the release that
  carries it.
