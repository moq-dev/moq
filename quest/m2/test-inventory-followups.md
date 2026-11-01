# [S] The test inventory follows every branch selection and covers JS benches

## Goal

`sh/gh/test-inventory.py` judges every check by the dispatcher branch its
caller actually selects, including the cargo-fuzz caller check, the
recipe-runs-tests check, and Python orchestrators' subcommands. It reports
each orphan once, and it checks JS bench scripts too.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7316](https://github.com/moq-dev/moq/pull/7316) (merged),
which finished the opt-in test inventory quest.
Milestone m2 (user: "use recommended for any CI/flakes").

[moq-dev/moq#7316](https://github.com/moq-dev/moq/pull/7316)'s review fixed a false pass: reaching any `gh nightly` group counted
every branch of `sh/gh/nightly.sh`. It now binds a `just` call's literal
arguments and follows only the `case "$1"` branch they select, but only
for the `#[ignore]` check. Left over:

- The cargo-fuzz caller check and the recipe-runs-tests check still treat a
  reached script as running all its branches. Apply the same selection.
- A Python orchestrator that dispatches on its first argument (argparse
  subcommands or an `if`/`match` on `sys.argv`) has all branches
  counted; follow only the selected one, or report it as unresolved.
- The failure output lists `benchmark_stream_import` twice. Deduplicate
  orphans by name and location.
- JS bench scripts: Nightly's JS bench list passes names through
  `{{ name }}`, which the walk skips. Teach it that pattern (or list the
  names literally) and require each JS bench a caller.

Each fix gets a case in `sh/gh/test-inventory.test.py` that fails on the
old walk. Optional, only if it falls out: trim `#[ignore]` reasons that
now duplicate the allowlist's owner (WGC, MediaCodec, NVIDIA).

Public API: none. Wire: none.
