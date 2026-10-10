# [L] Check and Test run on a self-hosted runner

## Goal

Check and Test for same-repo pull requests and merge-queue runs execute on a
self-hosted x86 NixOS runner with a warm Nix store and a local mbx cache that
only `main` writes. Fork PRs and every release workflow stay on GitHub-hosted
runners. Unset `vars.CI_RUNNER` and everything runs hosted, as today, so this
merges before the host exists.

Why: on the free org plan (20 concurrent jobs), PR jobs sat queued 20-50
minutes before running for 4-18, and about 4 minutes of each run was setup
(free-disk-space 45-80s, a ~6 GB mbx cache restore 150-210s). A persistent
host removes both. Platform, Android, WASM, and the other workflows are out of
scope; move them in follow-ups once this proves out.

## Plan

Decided in the 2026-10-09 planning session:

- **Trust boundary:** same-repo branches need push access, which already
  allows editing workflows, so only fork PRs are untrusted. Merge-queue runs
  are trusted too: a maintainer approved their code for `main`. Routing in
  `check.yml`:
  `runs-on: ${{ (github.event_name == 'merge_group' || !github.event.pull_request.head.repo.fork) && vars.CI_RUNNER || 'ubuntu-24.04-arm' }}`.
  Guarding is by review only, with a comment at each use; no lint.
- **Kill switch:** `vars.CI_RUNNER` (the runner label, `moq-ci`) set by the
  maintainer; unset falls back to hosted. No router job and no runner-status
  token.
- **Config home:** a NixOS module under `ci/runner/`, exported from
  `flake.nix` (e.g. `nixosModules.ci-runner`), which the host imports. Host
  setup instructions live in `ci/runner/README.md`.
- **Runners:** `services.github-runners`, ephemeral, `DynamicUser` and the
  module's default sandboxing, `noDefaultLabels`. Four `moq-ci` instances
  (`count` exposed so the host can tune it). One `moq-gpu` instance for
  [GPU CI](/quest/m1/gpu-ci.md) with `PrivateDevices=false` and
  `DeviceAllow` for `/dev/nvidia*`; it never takes `moq-ci` work.
- **Registration:** a dedicated GitHub App holding only the org
  "Self-hosted runners" permission, its key on the host. Register as org
  runners in a runner group restricted to `moq-dev/moq`; that is narrower than
  the repository Administration permission a repo-level runner needs. Never
  reuse moq-bot or a PAT.
- **Cache:** run `jdx/mr-boxington-cache` on the host with filesystem storage,
  bound to loopback. Writes require a GitHub OIDC token whose
  `job_workflow_ref` is `moq-dev/moq/.github/workflows/cache.yml@refs/heads/main`,
  so the cache warmer (push or dispatch) stays the single writer, matching the
  hosted design. Reads are open on loopback. Each job gets an empty
  `MBX_CACHE_DIR` and `target/`, pointed at the server in remote read-only
  mode. The server enforces the boundary; mbx's client-side mode narrowing is
  only a convenience. No static write token anywhere, since a same-repo PR can
  read secrets.
- **Warming:** add a self-hosted leg to `cache.yml` (`runs-on: moq-ci`, gated
  on `vars.CI_RUNNER` too) that runs the unscoped suite against the server. The
  existing ARM store keeps serving hosted fallbacks.
- **Workflow steps:** skip free-disk-space, the Nix installer, and the
  `rust-cache` action when `runner.environment == 'self-hosted'`. Put the
  kixelated cachix substituter in the module's `nix.settings`, since dynamic
  users are not `trusted-users` and cannot pass `extra-substituters`.

Verify before merging what can be checked without the host: `actionlint`,
the routing expression's three cases (fork, same-repo, merge group) with
`CI_RUNNER` set and unset, and that the module evaluates (`nix eval` or a
`nixosTest` that boots the cache server and rejects a write without a matching
token). End-to-end proof happens in [CI host](/quest/m0/ci-host.md).

Public API: none. Wire: none.

## Related

- [CI host](/quest/m0/ci-host.md) - the maintainer brings up the host this module configures
- [GPU CI](/quest/m1/gpu-ci.md) - the nightly NVIDIA job that runs on the `moq-gpu` instance
- [Merge queue](/quest/m1/merge-queue-settings.md) - once on, its runs route here too
