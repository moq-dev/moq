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
  allows editing workflows, so fork PRs are untrusted. So are Dependabot PRs:
  Dependabot pushes its branches into this repository, but they carry
  third-party build code nobody has reviewed yet, and `dependabot.yml`
  already treats them as untrusted. `pull_request.user.login` names the PR's
  opener, so a Dependabot PR stays hosted even after a maintainer pushes to its
  branch. Merge-queue runs are trusted because a maintainer enqueues every
  entry; a reviewed Dependabot bump is then no different from a human-authored
  dependency bump, which every job already builds. To keep that true, delete `.github/workflows/dependabot.yml` (its
  only job enables auto-merge on Dependabot PRs); a maintainer enqueues
  Dependabot PRs by hand (decided on review: the `merge_group` event cannot
  name its PRs' authors, and a router job was rejected). Deleting the workflow
  leaves auto-merge set on Dependabot PRs already open, so run
  `gh pr merge --disable-auto` on each before setting `CI_RUNNER`. Routing in
  `check.yml`:
  `runs-on: ${{ (github.event_name == 'merge_group' || (!github.event.pull_request.head.repo.fork && github.event.pull_request.user.login != 'dependabot[bot]')) && vars.CI_RUNNER || 'ubuntu-24.04-arm' }}`.
  Guarding is by review only, with a comment at each use; no lint.
- **Architecture:** the host is x86, while the hosted fallback is ARM. PRs on
  the host test x86; ARM stays covered by macOS (arm64) in `platform.yml` and
  the nightly ARM Linux jobs. The x86 and ARM caches never share entries.
- **Kill switch:** `vars.CI_RUNNER` (the runner label, `moq-ci`) set by the
  maintainer; unset falls back to hosted. No router job and no runner-status
  token.
- **Config home:** a NixOS module under `ci/runner/`, exported from
  `flake.nix` (e.g. `nixosModules.ci-runner`), which the host imports. Host
  setup instructions live in `ci/runner/README.md`.
- **Runners:** `services.github-runners`, ephemeral, `DynamicUser` and the
  module's default sandboxing, `noDefaultLabels`. Ephemeral resets each
  runner's user and work directory, but the Nix store and daemon persist and
  are shared by every instance, `moq-gpu` included. The GPU runner's stricter
  trigger rule protects which code it runs, not the machine; same-repo PR code
  shares that host by design. Four `moq-ci` instances
  (`count` exposed so the host can tune it). One `moq-gpu` instance for
  [GPU CI](/quest/m1/gpu-ci.md) with `PrivateDevices=false` and
  `DeviceAllow` for `/dev/nvidia*`; it never takes `moq-ci` work. Put the
  kixelated cachix substituter in the module's `nix.settings`, since dynamic
  users are not `trusted-users` and cannot pass `extra-substituters`. The
  shared store grows with every flake revision, so enable disk-aware
  `nix.gc` (with `min-free`/`max-free`) and keep a GC root on the current
  dev shell closure so collection never cools the warm store.
- **Registration:** a dedicated GitHub App holding only the org
  "Self-hosted runners" permission, its key on the host. Register as org
  runners in a runner group restricted to `moq-dev/moq`; that is narrower than
  the repository Administration permission a repo-level runner needs. Never
  reuse moq-bot or a PAT. `services.github-runners` takes a token file, not an
  App key, and an ephemeral runner re-registers on every restart, so a
  pre-start step mints a fresh installation token from the key into that file
  each time.
- **Cache:** run `jdx/mr-boxington-cache` on the host with filesystem blob
  storage and a durable metadata database (the default `memory://` loses the
  index on restart, orphaning the blobs; PostgreSQL via `services.postgresql`
  unless a persistent embedded backend is supported), bound to loopback. The
  server has no blob expiry on the filesystem backend, so a timer wipes blobs
  and metadata together once the store passes a size cap (or the flake hash
  changes); the next warmer run refills it. Writes require a GitHub OIDC
  token whose `workflow_ref`
  is `moq-dev/moq/.github/workflows/cache.yml@refs/heads/main`, with
  `repository`, `repository_owner_id`, `ref: refs/heads/main`, and
  `event_name` of `push` or `workflow_dispatch` also checked
  (`job_workflow_ref` is only meaningful for reusable workflows). The cache
  warmer stays the single writer, matching the hosted design. Its job needs
  `permissions: id-token: write` to mint that token. Reads are open on
  loopback. Each job gets an empty
  `MBX_CACHE_DIR` and `target/`, pointed at the server in remote read-only
  mode. The server enforces the boundary; mbx's client-side mode narrowing is
  only a convenience. No static write token anywhere, since a same-repo PR can
  read secrets.
- **Warming:** add a self-hosted leg to `cache.yml` (`runs-on: moq-ci`, gated
  on `vars.CI_RUNNER` too) that runs the unscoped suite against the server. The
  existing ARM store keeps serving hosted fallbacks.
- **Workflow steps:** skip free-disk-space and the Nix installer when
  `runner.environment == 'self-hosted'`. Keep the `rust-cache` composite
  action there, since its mbx install and `mbx setup` shim are the only route
  from Cargo into mbx; give it a self-hosted mode that points mbx at the
  server instead of restoring the hosted store.

The squash merge queue is active (ruleset 2420853, verified 2026-10-10;
#5291 passed through it). Its condition quest is complete. The workflow
removal and disabling previously enabled Dependabot auto-merge remain here;
do not wait for the superseded automatic-merge completion check.

Verify before merging what can be checked without the host: `actionlint`,
the routing expression's four cases (fork, Dependabot, same-repo, merge
group) with `CI_RUNNER` set and unset, and that the module evaluates (`nix eval` or a
`nixosTest` that boots the cache server and rejects a write without a token
and with a token from a PR ref). End-to-end proof happens in
[CI host](/quest/m0/ci-host.md).

Public API: none. Wire: none.

## Related

- [CI host](/quest/m0/ci-host.md) - the maintainer brings up the host this module configures
- [GPU CI](/quest/m1/gpu-ci.md) - the nightly NVIDIA job that runs on the `moq-gpu` instance
