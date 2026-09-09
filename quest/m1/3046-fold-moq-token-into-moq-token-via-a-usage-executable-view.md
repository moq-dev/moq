# [M] Retire the standalone moq-token binary

## Goal

`moq token` is the only spelling of the token CLI: one binary, one help tree,
one completion tree, one release artifact. After one deprecation release the
`moq-token-cli` crate, its release workflow, and its packaging are deleted.
The `moq-token` library is untouched.

## Plan

`moq-token-cli` is a lib+bin. Its `Args` is nested by `moq-cli` as `moq token`
(rs/moq-cli/src/main.rs:197-199, the only consumer, rs/moq-cli/Cargo.toml:101)
and wrapped by the standalone binary's own `Root` (rs/moq-token-cli/src/lib.rs:
12-22). The logic is shared; the surface is duplicated: two spec roots, two
help renderings, two completion trees, two release artifacts, and two places
for a flag to drift. The docs already point at `moq token` (#3557).

usage-rs 6.3.0 can dispatch a second root off argv0 (executable views,
`executable_views_emit_and_dispatch_from_argv0` in its tests/facade.rs), so
`moq-token` could survive as a renamed copy of `moq`. That keeps a name nobody
depends on and makes it an alias of the full media router, so the binary goes
instead.

1. Deprecation release: `moq-token` prints a notice naming `moq token` on
   every run and keeps working. Ship it as an ordinary `moq-token-cli` patch.
2. Delete the crate. Move `Args` and its subcommands into `moq-cli`, and
   remove `.github/workflows/moq-token-cli.yml`,
   `packaging/moq-token-cli/nfpm.yaml`,
   `.github/homebrew/Formula/moq-token-cli.rb.tmpl`, and the
   `moq-token-cli` triggers and entries in `.github/workflows/docker.yml:8`,
   `cachix.yml:7,47`, `alert.yml:37`, `release-brew.yml:22,152`, and
   `release-winget.yml:28,92`.
3. Grep the repo for `moq-token ` invocations and `moq-token-cli`, and fix
   every remaining doc, demo recipe, and install page.

Branch from `dev`, where the CLI lives on `usage`.

## Closes

- [#3046](https://github.com/moq-dev/moq/issues/3046) - close this issue when the quest finishes
