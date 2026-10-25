# [S] Share crane's dependency layer

## Goal

moq's Nix packages build their dependencies once, in one crane dependency layer that
every package reuses as its `cargoArtifacts`. Editing a workspace crate, kio
included, rebuilds no dependency, and `nix build` of a second package after the
first compiles only that package's own crates. Record in the PR how many crates
each case compiles, before and after.

## Plan

Decided with the maintainer on 2026-10-25, as the first step of making each crate
configuration compile once across moq's CI and release builds, measured in runner
CPU time. Today `nix/overlay.nix` builds a separate dependency layer per package,
and any change to that package's features, `cargoExtraArgs`, or `RUSTFLAGS`
rebuilds all of its roughly 330 dependencies.

- One dependency layer, built with the union of the features the packages use,
  passed to every package as `cargoArtifacts`.
- Move moq-relay's `-C force-frame-pointers=yes` into `.cargo/config.toml` so
  every build, Nix or cargo, compiles with the same flags and shares mbx keys.
  Frame pointers cost little on x86_64; measure aarch64 too.
- The dependency layer copies kio's real source into crane's dummy tree, because
  `[patch.crates-io] kio` points registry crates at the workspace member. That
  makes every kio edit rebuild every dependency. Restructure it so kio builds in
  the workspace layer, for example by giving the dummy tree a stub kio that
  exposes its API, or by building the crates that depend on the patched kio in
  the workspace layer.

kixelated/rust2's
[Nix build benchmark](https://github.com/kixelated/rust2/blob/main/quest/m1/bench-nix.md)
then compares this layering with crane plus mbx, crate2nix, and cargo with mbx,
and moq adopts the winner in a follow-up.

Public API: none. Release artifacts: built with frame pointers on every target,
not only moq-relay.
