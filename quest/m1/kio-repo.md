# [L] Move kio to its own repository, kixel-ai/kio

## Goal

`rs/kio` and `rs/kio/macros` move out of `moq` into a new public `MIT OR Apache-2.0` repository, `kixel-ai/kio`, with filtered history from `moq` (mirroring `kixel-ai/swole`). `moq` removes `rs/kio/`, switches its Cargo dependencies to `kixel-ai/kio`, updates its workspace, `justfile`, CI, release workflow, and repository map, and carries the pure-`kio` quests plus [the kio state-machine spikes epic](/quest/m9/kio-state-machines/README.md) into `kixel-ai/kio`'s quest tree.

## Plan

Decided 2026-11-25 (maintainer, `/quest-plan` on provable state-machine designs for `kio`):

- ✅ Move `rs/kio` to a dedicated `kio` repo, then run the state-machine spikes there (rejected: new project in `kixel-ai/rust2`, fresh blank-slate experiment repo leaving `rs/kio` in `moq`, keeping the spikes in `moq`'s `m9` lab first).
- ✅ GitHub repo `kixel-ai/kio`, moved now in `m1` (rejected: `moq-dev/kio` moved in `m1`, `kixel-ai/kio` moved after the release cut in `m4`). `kio` is a general-purpose async crate with no MoQ concepts (matching `kixel-ai/swole`, `crabx`, `rind`, and `rust2`), switching `moq` to depend on external `kio` is additive for `moq` callers, and moving in `m1` unblocks the spikes without waiting behind the release cut.

Implementation guidance (following `kixel-ai/rust2`'s [swole repo move](https://github.com/kixel-ai/rust2/blob/e0484e81/quest/m1/swole-repo.md)):

- Filter history with `git filter-repo` over `rs/kio/` and the carried quest files, moving `rs/kio/` to the new repository root (`Cargo.toml` workspace with `kio` and `macros/`). Rewrite bare `#N` references in commit messages to `moq-dev/moq#N`, and update package `repository` metadata to `https://github.com/kixel-ai/kio`.
- Copy `moq`'s reusable setup trimmed to what `kio` needs: Nix flake, direnv, `just ci`, `AGENTS.md`, `CONTRIBUTING.md`, `quest init`, org rulesets via dotfiles' `repo-settings`, and crates.io release automation for `kio-macros` then `kio`.
- Carry these pure-`kio` quests into `kixel-ai/kio`'s quest tree (dropping the `Required` link to this quest on the carried epic):
  - [the kio state-machine spikes epic](/quest/m9/kio-state-machines/README.md) and its three children (`pure-machine.md`, `reactor-macro.md`, `adversarial-sim.md`);
  - [Verus proves kio::log's commit protocol](/quest/m9/verus-kio-log.md);
  - [kio::Tasks tolerates a removed task's waker firing into its reused slot](/quest/m4/kio-tasks-waker-test.md);
  - [Miri's GenMC mode explores every schedule of kio's lock-free code](/quest/m5/miri-genmc.md);
  - [Drop kio's epoch fence once crossbeam fixes pin](/quest/m5/crossbeam-pin-fence.md).
- Keep `moq`-bench quests that measure `moq-relay` / `moq-net` in `moq` ([kio-waiter-spike.md](/quest/m3/kio-waiter-spike.md), [perf/kio-waiter-costs.md](/quest/m9/perf/kio-waiter-costs.md), [kio-channel-contention](/quest/m9/kio-channel-contention/README.md)), noting the repo boundary where needed.
- In `moq`: delete `rs/kio/`, point `Cargo.toml` workspace dependencies for `kio` and `kio-macros` at `kixel-ai/kio` (crates.io version or pinned git rev until the next `kio` publish), update `doc/setup/repository.md`, `justfile`, CI / Nightly / Miri scripts that reference `rs/kio`, and confirm `rust2-ts` / `rust2-ffi` pins do not assume a local `rs/kio` path.

Public API: none for `moq` callers (`kio` keeps its published 0.6.x API). Wire: none.

## Related

- [Spike provable state-machine designs for kio](/quest/m9/kio-state-machines/README.md) - the spikes unblocked by moving `kio` out of `moq`
- [Spike: Verus proves kio::log's commit protocol as written](/quest/m9/verus-kio-log.md) - carried into `kixel-ai/kio` at the split
