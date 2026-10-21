# [S] Smoke's flake bumps with its Playwright pin

## Goal

`test/smoke/flake.lock` stays current without a hand-made bump, and every
bump moves the npm `playwright` pin to the flake's `PLAYWRIGHT_VERSION` in
the same PR, so `just freshness` passes. If automating the pin costs more
than it saves, the coupled bump is documented where a maintainer will find
it when the update is due.

## Plan

From [moq-dev/moq#6169](https://github.com/moq-dev/moq/pull/6169).
Dependabot's nix ecosystem on main covers only `/`. #6169 gives the smoke
flake its own entry, but Dependabot updates only the lock: `freshness.sh`
then fails because the npm pin did not move. `test/smoke/README.md` already
says to bump both together by hand.

- Prefer automation: a workflow that follows a Dependabot smoke-flake PR
  and pushes the matching pin, or one scheduled job that bumps both.
- Whichever lands, a stale smoke flake must be visible, not silent.

Public API: none. Wire: none.

## Related

- [Playwright from nix](https://github.com/moq-dev/moq/pull/6116) - plans the Playwright pin this bump must follow
