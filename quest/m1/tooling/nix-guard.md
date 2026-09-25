# [S] Scoped checks refuse a host toolchain

## Goal

`just check`, `just fix`, and `just test` stop at once with a clear "run
inside `nix develop`" message when they run outside the Nix dev shell, instead
of failing deep in a build on a host toolchain difference. The repository
already requires the dev shell; this makes the requirement loud. An explicit
opt-out keeps a deliberate host run possible.

## Plan

- The trigger: `moq-tokio --all-features` builds jemalloc, whose configure
  adds `-Werror` to its `strerror_r` probes, so any host warning (likely
  `_FORTIFY_SOURCE` at `-O0`) makes it report "cannot determine return type of
  strerror_r". The dev shell disables fortify hardening, which is why CI
  passes. Reproduce it once outside the shell and confirm the cause from
  jemalloc's `config.log` before relying on it.
- Put the guard in `sh/dispatch.sh`, the one place the scoped verbs resolve,
  next to the existing `MOQ_STRICT` missing-tool check. How to detect the dev
  shell (`IN_NIX_SHELL`, a variable the flake sets, or a toolchain probe) is
  the implementer's call; prefer whatever direnv and `nix develop --command`
  both set.
- CI already runs inside the dev shell, so it is unaffected. Verify that a
  direnv-loaded shell passes the guard.

Public API: none. Wire: none.
