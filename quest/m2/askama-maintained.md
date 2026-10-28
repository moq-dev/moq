# [S] Askama uses a maintained TOML parser

## Goal

A stable Askama release replaces basic-toml, and the generator-only UniFFI
fork can use it without losing its custom template syntax. Upgrade the
generator dependency and remove RUSTSEC-2026-0374 from `deny.toml`;
`just rs audit` passes without that exception.

## Plan

Check the upstream [TOML dependency](https://github.com/askama-rs/askama/blob/main/askama_derive/Cargo.toml)
and crates.io releases. Main already uses toml, but 0.16.1 still uses
basic-toml. Keep Askama's `config` feature: UniFFI needs `askama.toml` for
its template syntax. Verify binding generation after the upgrade.

## Related

- [Dependency audit](/quest/m2/audit-deps.md) - recheck the rest of the dependency graph
- [Switch to rust2's UniFFI](/quest/m2/uniffi-fork.md) - askama reaches the lock only through `uniffi_bindgen`, which that switch moves to rust2
