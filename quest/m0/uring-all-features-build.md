# [S] moq-uring does not compile with --all-features

## Goal

`cargo clippy -p moq-uring --all-features --all-targets` compiles, so the
nightly `just rs features` gate covers `moq-uring` instead of failing on it.

## Plan

Enabling both `noq` and `quinn` puts two `quinn_proto` versions in scope, and
`rs/moq-uring/src/quic/quinn/connection.rs`'s test module refers to
`quinn_proto` unqualified, so the reference is ambiguous and the test target
does not build. `--all-features --lib` is clean; only `--all-targets` trips it.

Disambiguate the reference in the test module. Check whether the same
unqualified path appears anywhere else that only compiles under one of the two
features, since the ambiguity is invisible until both are on at once.

This is dev-only: `moq-uring` does not exist on main.

### How it was found

Verified while adding qlog support to the io_uring workers: reproduced on a
stashed tree at the merge base, so it predates that work. It is unrelated to
qlog and was deliberately not fixed there.

## Related

- [Worker metrics](/quest/m1/uring-metrics.md) - other moq-uring runtime work in the same crate
