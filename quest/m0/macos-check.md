# [S] just check passes on macOS

## Goal

`just check` on a clean `main` checkout passes on a macOS host inside the Nix
dev shell, as it already does on Linux.
[moq-dev/moq#8426](https://github.com/moq-dev/moq/pull/8426) fixed the
failures in scripts, tests, and Linux-only crate docs; two remain, both seen
on the maintainer's aarch64-darwin host on 2026-11-10 with
`quest gate nix develop --command just check` selecting the whole tree:

- `check cpp`: the build stops at
  `clang++: error: -Wl,-lintl: 'linker' input unused [-Werror,-Wunused-command-line-argument]`,
  after the `probe` builds pass, in a CMake build under
  `cpp/moq/build/test-*` whose compiler is the mbx `clang++` shim.
- `check py`: Sphinx, which treats warnings as errors, stops with
  `py/moq-rs/docs/index.md:147: WARNING: autosummary: stub file not found 'moq.media.catalog'. Check your autosummary_generate setting.`

Non-goal: a macOS runner on the PR gate (rejected in
[macos.md](/quest/m0/nightly/macos.md)).

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of #8392 and #8394).
Milestone: ✅ m0 (rejected: a later milestone). Narrowed 2026-11-10
(maintainer): ✅ land #8426 and keep this quest for what remains (rejected:
widen #8426 again).

Causes are not yet diagnosed:

- `check cpp`: `-lintl` is gettext's library, which the Nix darwin stdenv
  adds to link flags; it likely reaches a compile-only clang++ step through
  `NIX_LDFLAGS` or the CMake flags `cpp/` passes. Find which step gets it and
  keep link flags off compile steps rather than dropping `-Werror`.
- `check py`: check whether `moq.media.catalog` exists in the built module on
  macOS (a cfg-gated or feature-gated binding would explain a missing stub),
  then align the docs index with what the module exports there.

Verify on a macOS host (a maintainer machine or a manual Platform dispatch),
and on Linux.

Public API: none. Wire: none.
