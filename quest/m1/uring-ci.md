# [XS] io_uring tests run on PRs

## Goal

A PR touching io_uring code runs `just rs uring` (the io_uring clippy and the
moq-uring and moq-relay nextest runs with the `io-uring` feature), so a
regression there fails the PR instead of the next nightly.

## Plan

Today `rs uring` runs only in `.github/workflows/nightly.yml`, kernel-gated to
6.12 and later. #5244 resolved a merge conflict in `rs/moq-relay/src/uring.rs`
that no PR check compiled.

- Add a PR job path-filtered to `rs/moq-uring/**`, `rs/moq-relay/src/uring*`,
  and the shared code they build on, following how other path-filtered PR
  workflows in `.github/workflows/` are set up.
- ✅ Decided 2026-10-13: fail loud. The job runs on a runner with a 6.12+
  kernel and errors if the kernel is older, rather than skipping. Not chosen:
  compile and lint only on PRs, tests nightly.
- Keep the nightly entry unless the PR job covers every path it does.

Public API: none. Wire: none.
