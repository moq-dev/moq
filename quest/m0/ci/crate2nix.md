# [M] Build moq's Nix packages with crate2nix

## Goal

moq's Nix packages build one derivation per crate through crate2nix instead of crane,
so `nix.moq.dev` serves every compiled crate to any runner and a lockfile bump or a
workspace edit rebuilds only the crates it changes. Contributors without Nix are
unaffected: cargo stays the build they use, and nothing they run needs Nix. Record
in the PR the instructions or CPU time for a clean build, a second package, the
moq-net edit, and a lockfile bump, before and after.

## Plan

Decided with the maintainer on 2026-10-26, from kixelated/rust2's
[Nix build benchmark](https://github.com/kixelated/rust2/pull/63). That benchmark
measured billions of instructions across a clean build, a second package, an edit
and a lockfile bump:

| | crane | crane with mbx | crate2nix | cargo with mbx |
| --- | ---: | ---: | ---: | ---: |
| total | 14,756 | 9,894 | 12,461 | 9,692 |
| edit | 1,365 | 1,351 | 1,008 | 1,276 |
| lockfile bump | 3,681 | 1,123 | 672 | 1,113 |

On the builds CI repeats, crate2nix was cheapest, and it caches through the Nix store
with no builder configuration. crane with mbx needs `extra-sandbox-paths` and a
persistent store on every builder.

- **Explore first.** Prototype crate2nix on moq-relay and moq-cli before switching
  every package, and keep crane until the numbers hold on moq's own CI.
- **No Nix required.** The generated `Cargo.nix` is committed so Nix users build
  without generating it. CI regenerates it and fails when it is stale. Prefer
  regenerating it automatically, for example a CI job that commits the update on a
  Renovate or lockfile PR, so a contributor without Nix never has to.
- **Cold build.** crate2nix builds build scripts and proc macros at opt-level 3,
  where cargo uses 0, which makes a clean build 29% dearer. Match cargo.
- **Flags.** Move moq-relay's `-C force-frame-pointers=yes` into
  `.cargo/config.toml` so Nix and cargo builds use the same flags; measure aarch64.
- **Also measure** `codegen-units = 1` for the release profile, which cut crate2nix's
  builds 20% to 45% in the benchmark.
- Replaces the crane layering quest, which only improved the second-package case:
  with crane, a lockfile bump still rebuilds every dependency.

Public API: none. Release artifacts: built by crate2nix, with frame pointers on
every target.

## Related

- [Unify dependency features](/quest/m0/ci/hakari.md) - lets packages share more crate derivations
