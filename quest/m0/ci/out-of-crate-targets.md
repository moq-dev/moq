# [S] Branch checks cover out-of-crate targets

## Goal

When a branch changes only a file that a Cargo target points at from outside
its crate, such as the Rust interop clients under
`test/interop/clients/rust/`, `just check` and `just fix` format and lint that
crate.

## Plan

Found by [moq-dev/moq#7015](https://github.com/moq-dev/moq/pull/7015).
`rs/moq-cli/Cargo.toml` builds those clients as examples through `path =
"../../test/interop/clients/rust/*.rs"`, but `sh/rs/select.sh` seeds crates
only from paths under `rs/` and `demo/` (and the `rs` scope in
`sh/dispatch.sh` matches the same way), so those files reach neither rustfmt
nor clippy. Seed from each target's `src_path` in the `cargo metadata` the
selector already reads instead of adding a moq-cli special case, and cover it
in the selector's tests.

Decided 2026-10-28 (maintainer): the draft-18 FETCH cancel spelling the same
PR noted (a reset from Rust, a FIN from JS) is not a quest, since both sides
accept either as a draft-18 cancel (`fin_cancels` in
`rs/moq-net/src/ietf/request_stream.rs`).

Public API: none.
