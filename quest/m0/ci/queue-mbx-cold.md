# [M] The merge queue's Rust builds hit what earlier queue runs cached

## Goal

A `queue.yml` run restores from mbx's local store what earlier queue runs
compiled, so a full run's misses fall by at least half from the 260 misses and
105 bypasses in the baseline below, and the PR that closes this records the
counts from the same kind of run before and after.

## Plan

Found 2026-11-22 while cutting `just check` CPU in
[Cut Rust just check CPU](https://github.com/moq-dev/moq/pull/9413). The queue
runs on the self-hosted `moq-queue` runner with mbx's local backend, so `0 B
downloaded` in its summaries is expected: no remote is configured. The signal
is in the local hits. Across five queue runs on 2026-11-21 and 2026-11-22, some
hit well: in 45024192478 one job had 1687 hits and 343 misses. Others
started cold in nearly every job: 45028239206 had 0 hits and 336 misses in its
largest job and 0 hits in most of the rest, and 45029886165 had 0 hits in
every job. Cold and warm runs alternate, which points at the store rather than
unstable keys.

- Find why a run starts cold: more than one runner container, each with its own
  store, a gc budget that evicts between runs (the largest job alone stores 4.0
  GiB), or a cache directory that resets with the container. Read
  `mbx analyze` and the mbx config on the runner host (`nixos`).
- The deployed runner is the systemd-nspawn container `container@moq-ci`
  (decided 2026-10-25 in [CI](/quest/m0/ci/README.md)), not the
  `nix/runner` VM, and its host config may live in dotfiles rather than this
  repository. Fix it there if so, and record the change here.
- Read the bypass reasons of the jobs that bypass 73 to 126 compilations, and
  cache what mbx can.

Decided 2026-11-22 (maintainer, pick of #9413): ✅ kept as a follow-up.

Public API: none. Wire: none.

## Related

- [The Rust cache stops paying for uploads R2 refuses](/quest/m0/ci/mbx-failed-puts.md) - the hosted jobs' R2 cache, not the queue's local store
