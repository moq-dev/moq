# [S] Incremental compilation pays off for moq-net

## Goal

A one-line body edit in moq-net costs less to re-check than a clean build,
or moq-net's dev profile stops paying for incremental state it can't reuse.
The PR reports the before and after numbers.

## Plan

Found by the [poll-macro spike](https://github.com/moq-dev/moq/pull/7057)
(planned 2026-10-28): a one-line edit in `rs/moq-net/src/lite/subscriber.rs`
(8.5k lines) costs 45 to 57 G instructions to re-check incrementally, more
than a clean incremental check (44 to 47 G) and about twice a clean
non-incremental one (26.5 G).

- Profile the edit rebuild with `-Zself-profile` to find what incremental
  recomputes.
- Then choose between `incremental = false` for moq-net and splitting its
  largest files, and measure the edit rebuild again.
- Count instructions with the real cargo binary (the mbx cargo shim serves
  cached builds), `perf stat --no-scale -e instructions:u`, summed over both
  core types; without `--no-scale` perf inflated counts by up to 20% on this
  hybrid CPU.

Public API: none. Wire: none.
