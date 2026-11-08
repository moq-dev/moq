# [S] moq-cli in nixpkgs

## Goal

`nix-shell -p moq-cli` installs the released `moq` binary from upstream
nixpkgs with the full release feature set (capture, play, and the hardware
encoders), kept current by nixpkgs-update from our `moq-cli-v*` tags.
`doc/setup/install.md` gives the command once it reaches nixpkgs-unstable.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan):

- Should the nixpkgs moq-cli wait for cli-packaging's `release` feature?
  - Don't wait
  - ✅ Wait for cli-packaging
- The maintainer added "moq-cli only when we're happy with the feature set"
  and left the bar to the planner. Decided: the bar is
  [Ship capture and playback](/quest/m3/cli-packaging.md) merged, since that
  is when every distribution builds with `--features full`.

Follow [moq-relay in nixpkgs](/quest/m3/nixpkgs-relay.md)'s process: prepare
a branch on kixelated/nixpkgs with `Assisted-by:` trailers, then wait for the
maintainer to review and open the PR. The maintainer-list entry already exists
by then.

The attribute is `moq-cli`, because nixpkgs' `moq` is an unrelated Go mocking
tool ([matryer/moq](https://github.com/matryer/moq)) that also installs
`bin/moq`. Set `meta.mainProgram = "moq"`. Build with `--features full`
and the `jemalloc` feature our overlay uses. `vaapi` and `nvidia` dlopen
their driver libraries, so make them findable at runtime (for example
`addDriverRunpath` for libcuda, and libva on the runpath). Prove it with
`moq devices` and `moq play` on a NixOS host, not only `--help`.

## Required

- [Ship capture and playback](/quest/m3/cli-packaging.md) - the `release` feature set that the nixpkgs package builds
- [moq-relay in nixpkgs](/quest/m3/nixpkgs-relay.md) - kixelated's maintainer entry and the package conventions
