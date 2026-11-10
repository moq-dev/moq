# [XS] The C bindings accept MoqInstance again

## Goal

`check c` in `just check` passes on `main`. The C bindings generator stops
refusing `MoqInstance`, whose `#[uniffi::export(Debug, Display, Eq, Hash)]`
in `rs/moq-ffi/src/instance.rs` exports a `Debug` trait method the generator
can't render. Every branch's local `just check` fails on it until then.

## Plan

Planned 2026-11-09 (maintainer, /quest-unblock), after
[moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375)'s iterate found
`check c` failing identically on a clean `origin/main`. It came in with
[moq-dev/moq#8366](https://github.com/moq-dev/moq/pull/8366) (minted instances
are a typed flag), merged 2026-11-10 03:04 UTC; list it under the fix's
`## Caused by`.

Either stop exporting `Debug` (keep `Display`, `Eq`, and `Hash` if the other
bindings use them), or have the C generator handle or skip exported trait
methods the way it handles the others. Prefer the change that keeps every
binding at the same functionality, as the root `AGENTS.md` requires.

Public API: possibly a binding's debug string for `MoqInstance`. Wire: none.
