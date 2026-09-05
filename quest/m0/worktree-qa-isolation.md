# [M] Worktree QA isolation

## Goal

Two worktrees can build, run relay/browser tests, and clean up independently.
The checkout's base, Git metadata access, test endpoints, and owned processes
are explicit, so one task cannot accidentally test or stop another's server.

## Plan

`test/smoke/smoke.sh` defaults to port 4443 and `test/wasm/run.sh` uses a fixed
span starting at 4460. Both offer overrides, but allocation is left to callers.
A linked checkout's actual Git directory can live outside its writable source
root, as it does for Codex worktrees linked to the main repository.

- Add a reusable worktree setup/inspection command that records the freshly
  fetched base SHA and branch upstream. Follow CONTRIBUTING's main/dev rule;
  never reset or rebase an existing dirty checkout as part of setup.
- Resolve `--git-dir` and `--git-common-dir`, and report the exact metadata
  access needed for fetch, branch creation, and rebase. Use the host's supported
  permission mechanism; source write access alone is insufficient.
- Give each test run a private directory and endpoint manifest. Prefer binding
  port zero and reporting the bound address, or a held allocation if the relay
  cannot yet report it. Checking a free port and then releasing it is a race.
- Track process ownership through a supervisor or process groups that the run
  creates. Cancellation and startup failure must reap only that run's children,
  even if a PID is later reused. Print the browser URL and a rerun command.
- Document normal teardown and explicit retained-debug-session teardown.
  Preserve dirty/untracked work and active builds; cleanup must not launch
  machine-wide cache or Nix garbage collection.

Acceptance: start smoke and WASM runs concurrently from two worktrees, cancel
one during startup and another during playback, and verify the survivor still
works with no leaked listeners or children. In a disposable linked repository,
verify stale-base and denied-metadata diagnostics without changing its refs.

## Related

- [Verification preflight](/quest/m0/verification-preflight.md) - reports capabilities before expensive work
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - retains evidence after process cleanup
