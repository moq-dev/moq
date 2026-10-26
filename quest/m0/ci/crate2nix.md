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
[Nix build benchmark](https://github.com/kixelated/rust2/pull/63), in billions of
instructions. Each scenario builds moq-relay, except the second package (moq-cli
after moq-relay); the total sums the four.

| | crane | crane with mbx | crate2nix | crate2nix, 1 codegen unit | cargo with mbx |
| --- | ---: | ---: | ---: | ---: | ---: |
| clean | 3,681 | 3,377 | 4,736 | 3,695 | 3,378 |
| second package | 5,926 | 3,945 | 5,951 | 4,902 | 3,924 |
| moq-net edit | 1,365 | 1,351 | 1,008 | 753 | 1,276 |
| lockfile bump | 3,681 | 1,123 | 672 | 496 | 1,113 |
| total | 14,756 | 9,894 | 12,461 | 9,846 | 9,692 |

Neither crate2nix column matches moq's `[profile.release]`, which every other
column builds with: `codegen-units = 1` and fat LTO. buildRustCrate reads no Cargo
profile. The benchmark gave it 16 codegen units, and the separate run used
buildRustCrate's default of 1; neither ran LTO, which every crane and cargo build
pays at the final link, edits and bumps included. The 29% cold-build penalty
vanishes at one codegen unit, so it came from that mismatch.

crate2nix was still cheapest on the edits and lockfile bumps CI repeats, and it
caches through the Nix store with no builder configuration. crane with mbx needs
`extra-sandbox-paths` and a persistent store on every builder.

- **Match the release profile first.** crate2nix's builds match `[profile.release]`
  before any comparison: one codegen unit, fat LTO on the final link, stripped
  symbols, the per-package overrides, and cargo's opt-level 0 for build scripts and
  proc macros where buildRustCrate uses 3. The before and after numbers come from
  those matched builds; LTO will raise crate2nix's edit and bump costs above the
  table's.
- **Explore first.** Prototype crate2nix on moq-relay and moq-cli before switching
  every package, and keep crane until the matched numbers hold on moq's own CI.
- **No Nix required.** The generated `Cargo.nix` is committed so Nix users build
  without generating it. Any change to a manifest, the lockfile, or a crate's
  targets can make it stale, so CI regenerates it on every PR and fails when it
  differs. A bot commits the regenerated file to the PR's branch whenever it can
  push there, Renovate's included; for a fork that blocks maintainer edits, the
  failure carries the update as a patch to `git apply`. The PR's manifests are
  untrusted, so generation runs without write access, and the job that pushes
  takes only `Cargo.nix` from it.
- **Flags.** Move moq-relay's `-C force-frame-pointers=yes` into
  `.cargo/config.toml` so Nix and cargo builds use the same flags; measure aarch64.
- Replaces the crane layering quest, which only improved the second-package case:
  with crane, a lockfile bump still rebuilds every dependency.

Public API: none. Release artifacts: built by crate2nix, with frame pointers on
every target.

## Related

- [Unify dependency features](/quest/m0/ci/hakari.md) - lets packages share more crate derivations
