# [XS] Switch moq's kio dependency from git to crates.io

## Goal

`Cargo.toml` (and `bench/fanout/*/Cargo.toml`) depend on a published crates.io release of `kio` from `kixel-ai/kio` instead of a `git` revision, so `cargo publish` can publish `moq`'s crates during the [release cut](/quest/m3/release/cut.md).

## Waiting

- kixel-ai/kio: a `kio` release past `0.6.4` (planned as `0.7.0`) is on crates.io. `kio-macros` `0.1.0` already is (published 2026-11-26). The release goes out when release-plz's release PR merges, which [kixel-ai/kio#21](https://github.com/kixel-ai/kio/pull/21) sets up and which waits on an App token for its `release-pr` environment. Check with `curl -sA kio-check https://crates.io/api/v1/crates/kio | jq -r .crate.max_stable_version`.

## Required

- [kio main](/quest/m0/kio-main.md) - moq builds and passes on the kio `main` that `0.7.0` will ship, including the moq-transport ingest fix its new scheduler exposed

## Plan

Found while moving kio out to `kixel-ai/kio` ([moq-dev/moq#9675](https://github.com/moq-dev/moq/pull/9675)): `Cargo.toml` pins `kio` to `kixel-ai/kio`'s git rev because `main`'s `kio` carries changes past the `0.6.4` published from `release`. Cargo refuses to publish any crate that depends on a git-only dependency, so this sits in `m0` as a direct `Required` blocker of [cut.md](/quest/m3/release/cut.md) (moved from `m2` on 2026-11-26 so it stops blocking the five pre-cut release reviews).

`0.6.4` cannot stand in: it predates `kio-macros`, so it has no `#[kio::wrapper]`, `#[kio::machine]`, or `kio::clock`, which moq uses throughout. Once [kio main](/quest/m0/kio-main.md) has moved moq onto kio's `main` and the release is out:

- Replace the `kixel-ai/kio` git pin in `Cargo.toml` and `bench/fanout/{session,model,group}/Cargo.toml` with the published crates.io version, and drop the pin from `rs/workspace-hack/Cargo.toml` via `cargo hakari generate`.
- Remove `"https://github.com/kixel-ai/kio"` from `allow-git` in `deny.toml`.
- Regenerate `Cargo.lock`, `demo/carla/bridge/Cargo.lock`, `bench/fanout/session/Cargo.lock`, and `nix/Cargo.nix` (`just rs cargo-nix`, after dropping the git entries from `nix/crate-hashes.json`).

Public API: none. Wire: none.
