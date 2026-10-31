# [XS] The workspace-hack refuses trybuild edges and spares non-media builds

## Goal

After the cargo-hakari workspace-hack lands: `sh/rs/hakari.py` fails when a
crate that dev-depends on trybuild also dev-depends on `workspace-hack`; a
Linux test build of a crate with no media dependency needs neither
libpipewire nor libclang; and `cargo-hakari` is on PATH in main's dev shell.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985). Milestone: ✅ m0 (user: "use recommended for any CI/flakes").

- **trybuild edge**: trybuild copies a crate's dev-dependencies into the
  project it compiles each UI case in, so `kio-macros`' hack edge made
  `kio-macros::ui` compile the whole hack (88-120 s), time out, and starve
  the tests beside it; #6985 was ejected from the land queue for it
  ([run 41074871604](https://github.com/moq-dev/moq/actions/runs/41074871604))
  and fixed it by excluding `kio-macros` by hand. Make `sh/rs/hakari.py`
  derive the rule: a crate with `trybuild` in its dev-dependencies must not
  carry the `workspace-hack` edge, with a test that plants one.
- **libpipewire**: CodeRabbit flagged on #6985 that the hack's unified
  features pull PipeWire (through cpal) and libclang into ordinary Linux
  test builds of crates that use no media. The dev shell provides both, so
  only builds outside it notice. Recommended: exclude the pipewire/cpal
  feature from the hack with hakari's final-excludes, then measure that the
  cache hit rate across jobs barely moves. If it does move, bring the
  numbers to the maintainer instead.
- **Dev shell**: confirm `nix develop` on main puts `cargo-hakari` on PATH
  (older worktree shells lacked it), so `just rs fix` and the lint's hint
  work; add it to the shell if not.

Public API: none. Wire: none.

## Required

- [Unify dependency features with cargo-hakari](/quest/m0/ci/hakari.md) - its PR, #6985, adds the workspace-hack and `sh/rs/hakari.py`
