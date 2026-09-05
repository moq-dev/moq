# [M] Verification environment preflight

## Goal

An agent starting in a fresh worktree can tell which checks it can actually
run, and why a required capability is unavailable, before starting a build.
A successful local command with skipped tools is never presented as complete
verification.

## Plan

The root `justfile` already has `_tools` and `MOQ_STRICT`; extend that source
of truth instead of maintaining another tool list. `.codex/hooks/direnv.sh`
can return successfully without exporting an environment, and a binary on
PATH does not prove that its daemon, cache, browser, or device is usable.

The audit reproduced a concrete version mismatch: local Bun 1.2.23 parses the
YAML key `on` as `true`, so `.github/scripts/alert.sh check-coverage` cannot read
the workflow trigger. Installed Nix-store Bun 1.3.13 preserves `on`. The same
workflow file therefore fails the fallback check despite Bun being on PATH.
Check required version/behavior, including this small parser fixture, rather
than only the executable name.

- Add a bounded `just doctor` recipe with human-readable and structured output:
  selected base and scope, tool paths and versions, dev-shell identity, Cargo
  wrapper and target directory, writable scratch/cache locations, and disk
  headroom. Classify available, missing, denied, and timed-out separately.
- Probe Nix evaluation, a tiny compile through the selected Cargo wrapper,
  loopback TCP/UDP bind, and the pinned Playwright browser launch. Probe only
  capabilities needed for the requested suite; do not bootstrap every language.
- Check GitHub read access independently from shell network access. Report
  whether PR checks, logs, and artifacts can be read without dumping credentials.
- Make the existing session hook surface its setup result and log location.
  Provide a documented Codex local-environment action calling the same recipe;
  keep the shell command usable by other agents and humans.
- Separate diagnosis from installation or permission changes. Emit the narrow
  required path/socket/capability rather than recommending unrestricted access.

Acceptance: exercise missing tools, denied Nix/cache access, an unavailable
browser, and a healthy fresh checkout. Each returns within its declared budget
and identifies the affected suites. Strict verification refuses an incomplete
required scope; a diagnostic run can still report all independent results.

The setup entry point follows the documented
[Codex local environments](https://learn.chatgpt.com/docs/environments/local-environment)
mechanism; verify the installed app's generated configuration when implementing.

## Related

- [Worktree QA isolation](/quest/m0/worktree-qa-isolation.md) - owns shared Git metadata and test resource lifecycle
- [Merge evidence](/quest/m0/merge-verification-evidence.md) - records unavailable checks alongside executed results
