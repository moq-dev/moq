# [S] Long test runs keep their built binaries

## Goal

A long `just test interop` or wire-compat run never fails because the
binaries it built were removed from the shared Cargo target mid-run, such
as by mbx pruning its managed build storage. A one-off Cargo launch failure
with `Argument list too long` is traced to its cause and fixed, or shown to
be host-only and handed to the host's config.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7211](https://github.com/moq-dev/moq/pull/7211). Milestone: ✅ m0 (user, "all recommended" for quests 1-8).

- **Target removed mid-run**: the 2026-10-31 run reported the shared
  `target/` deleted by mbx under long interop and wire-compat runs. mbx
  links each checkout's target to managed storage that prunes itself
  (`mbx gc` collects managed targets to fit its budget), and
  `test/interop/interop.sh:546` resolves `TARGET_BASE` once, then runs
  `moq-relay`, `moq-cli`, its examples, and moq-c's library from there for
  the whole matrix; `test/interop/compat.sh:71-72` does the same. Confirm
  from mbx's logs or `mbx gc --dry-run` which collection removes them.
  Then either copy what the run executes into the run's own directory
  (`$HARNESS_RUN`) right after building, which needs no mbx change, or use
  an mbx lease or pin if mbx offers one. Prefer whichever keeps one rule for
  every long-running script.
- **`Argument list too long`**: seen once in an interop Cargo launch
  (#7211). [moq-dev/moq#7414](https://github.com/moq-dev/moq/pull/7414)
  and [moq-dev/moq#7200](https://github.com/moq-dev/moq/pull/7200) hit the
  same error in the host's cache wrapper and traced it to recursive shell
  initialization through `BASH_ENV`, which grew the environment until exec
  failed; unsetting `BASH_ENV` cleared it. Check whether any script in this
  repository re-sources itself or appends to an exported variable per shell.
  If the growth is only the host's `BASH_ENV`, record that in the PR and
  leave the fix to the host's config.

Public API: none. Wire: none.
