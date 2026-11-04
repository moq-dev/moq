# [S] One typed unsupported-CPU error

## Goal

moq-auth's and moq-hls's `Error::UnsupportedCpu` and moq-crypto e2ee's
`Error::Unsupported` carry the shared
`moq_crypto::provider::UnsupportedCpu` instead of a `String`, converted with
`?`, and each crate's own `rind::cpu::check()` wrapper is gone
(`unsupported_cpu` in `rs/moq-auth/src/error.rs` and
`rs/moq-hls/src/error.rs`, and the `map_err` in
`rs/moq-crypto/src/e2ee/credential.rs`). A caller tells a CPU refusal from
any other failure by its type, the same way in every crate.

## Plan

Planned 2026-11-04 with the maintainer, from the suggested follow-up of
[moq-dev/moq#7873](https://github.com/moq-dev/moq/pull/7873)
([Transport on rind](/quest/m1/rind/transport.md)). That PR adds the
`provider` feature to moq-crypto, holding `provider::UnsupportedCpu`
(`Clone + Copy + Eq`, wrapping rind's `MissingFeature` privately), and moves
moq-quic, moq-tokio, qmux and moq-uring onto it as
`UnsupportedCpu(#[from] ...)` with `#[error(transparent)]`. This quest does
the same for the three crates it left, so it starts once that PR lands.

- **The type must not need rustls.** In #7873, `provider` pulls in rustls,
  since it also holds `provider()` and `install_default()`. moq-auth's core
  build has no rustls (only its `client` feature does), and the e2ee feature
  doesn't either. Recommended: put `UnsupportedCpu` and `check()` where they
  need only rind, such as a moq-crypto feature that `provider` and `e2ee`
  both enable, and keep `provider` for the rustls half. The type's path
  may then move out of `provider`, which nothing released uses yet. Check with
  `cargo tree` that moq-auth's and moq-crypto's default and
  `--no-default-features` graphs gain no rustls.
- moq-hls doesn't depend on moq-crypto today and gains it for this type.
- moq-rtmp kept `rind::cpu::check()` behind `io::Error` in #7873 only
  because `provider` needs rustls. If the split above lands, move it onto
  the typed error too, or record why not.
- Keep each variant's name: `UnsupportedCpu` in moq-auth and moq-hls. Rename
  e2ee's `Unsupported` to `UnsupportedCpu` too, since it only ever means a
  CPU refusal, so all three read alike.
- Docs naming these variants (`rs/moq-hls/src/lib.rs`, the moq-hls export
  and server rustdoc) follow the change.

Public API: breaking for moq-auth, moq-hls and moq-crypto (the variants'
payload type, and e2ee's variant name). Each gets a `## Unreleased` note in
its `MIGRATE.md`. Wire: none.

## Required

- [Transport on rind](/quest/m1/rind/transport.md) - adds `moq_crypto::provider::UnsupportedCpu`, which this wraps
