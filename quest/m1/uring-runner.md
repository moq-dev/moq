# [S] io_uring tests on a 6.12+ runner

## Goal

The io_uring relay tests actually run nightly instead of skipping. Today
GitHub-hosted runners are below the 6.12 kernel floor, so `just rs uring`
compiles the tests and every one of them skips. The io_uring drain test was
broken on the drain line and nothing noticed.

## Plan

Run the nightly `rs uring` recipe on a self-hosted Linux runner with kernel
6.12+, registered on the maintainer's host. Keep the GitHub-hosted entry only
if it still catches something the self-hosted one does not. Otherwise move it.

A self-hosted runner on a public repository must never run untrusted code.
Gate the job to `schedule` and `workflow_dispatch` on `main` (never
`pull_request` from forks), give it a label only that job selects, and keep its
permissions read-only. Check GitHub's self-hosted runner security guidance
before wiring it.

Fail loudly when the runner's kernel is below the floor instead of letting the
tests skip, so a runner that regresses reports it. Mind the host's memlock limit,
which `a05a2d5a5` sized the io_uring tests against.

## Required

- A self-hosted runner with Linux 6.12+ is registered for moq-dev/moq on the maintainer's host
