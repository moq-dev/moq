# [S] The merge gate runs send-depth

## Goal

A PR whose change deepens moq-net's native `Send` proofs past the default
recursion limit fails the merge gate, instead of turning Nightly red after it
lands. The check runs on a pinned nightly from the dev shell, so a toolchain
release alone never fails it.

## Plan

`just rs send-depth` runs only in Nightly, on whatever nightly
`dtolnay/rust-toolchain` installs. The gate never built it, so a deeper IETF
future failed Nightly every night from 2026-10-25 until
[moq-dev/moq#6890](https://github.com/moq-dev/moq/pull/6890). Land this after
that fix, since the check is red on `main` until then.

Decided 2026-10-28 (maintainer): gate it, because it is cheap. Measured on
2026-10-28: about 14 s cold and 9 s warm on a 24-core host, against a
21-minute median for the gate runs that select moq-net (26% of 144 runs
since 2026-10-25).

- Pin a dated nightly in the flake (rust-overlay already provides one) and
  have `sh/rs/send-depth.sh` use it instead of rustup's latest; Nightly's
  send-depth job uses the same pin. Bump the pin on purpose, like stable.
- Run it from `sh/rs/select.sh` beside `net-features`:
  `if wants moq-net; then just rs send-depth; fi`, so it runs only when
  moq-net or a workspace dependency of it changes.

Public API: none. Wire: none.

## Related

- [Measure the parallel frontend](/quest/m0/ci/nightly-levers.md) - also builds on a pinned nightly
