# [XS] A local suite's environment does not depend on where it starts

## Goal

A suite run through `sh/local.sh` gets the same environment whichever
directory it starts from, so a local run reproduces CI's logging and timing.

## Plan

Found 2026-11-16 while reproducing
[the interop FETCH flake](https://github.com/moq-dev/moq/pull/8894):
`sh/local.sh` runs `sh/local.ts` under `bun`, which loads `.env` from the
working directory. The repository's `.env` sets `RUST_LOG="debug"` and
`RUST_BACKTRACE=1`, so a suite started from the repository root runs every
Rust process at debug, while `just test interop` (whose recipes run in `test/`)
and CI run at the default info. Debug logging slows every relay and client
enough to change which races a run can hit.

Recommendation: run `sh/local.ts` with `bun --no-env-file`, as `quest` itself
runs, and delete the root `.env` if nothing else reads it (it dates from
[moq-dev/moq#928](https://github.com/moq-dev/moq/pull/928)). A developer who
wants debug output sets `RUST_LOG` in their shell.

Public API: none. Wire: none.
