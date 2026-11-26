# [XS] Switch moq's kio dependency from git to crates.io

## Goal

`Cargo.toml` (and `bench/fanout/*/Cargo.toml`) depend on a published crates.io release of `kio` from `kixel-ai/kio` instead of a `git` revision, so `cargo publish` can publish `moq`'s crates during the [release cut](/quest/m3/release/cut.md).

## Waiting

- kixel-ai/kio: `kio-macros` `0.1.0` and a new `kio` release past `0.6.4` are published to crates.io (tracked in `kixel-ai/kio`'s `quest/m1/crates-io-publish.md`). Verify with `curl -sA kio-check https://crates.io/api/v1/crates/kio-macros` returning 200 and `kio` showing the new version.

## Plan

Found while moving `rs/kio` to `kixel-ai/kio`: `Cargo.toml` pins `kio` to `kixel-ai/kio`'s git rev because `kio-macros` is not yet on crates.io and `main`'s `kio` carries changes past the `0.6.4` published from `release`. Cargo refuses to publish any crate that depends on a git-only dependency, so this sits in `m2` ahead of the cut.

- Replace `kio = { git = "https://github.com/kixel-ai/kio", rev = "..." }` in `Cargo.toml` and `bench/fanout/{session,model,group}/Cargo.toml` with the published crates.io version.
- Regenerate `Cargo.lock`, `demo/carla/bridge/Cargo.lock`, `bench/fanout/session/Cargo.lock`, `rs/workspace-hack/Cargo.toml`, and `nix/Cargo.nix`.

Public API: none. Wire: none.
