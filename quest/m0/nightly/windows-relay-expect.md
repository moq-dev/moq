# [XS] Windows builds moq-relay's tests

## Goal

Nightly's Windows job builds and runs the test suite again. Since 2026-10-25,
`cargo test --no-run` fails there with "this lint expectation is unfulfilled"
at `rs/moq-relay/src/relay.rs:871`.

## Plan

The module-level `#[expect(clippy::disallowed_methods)]` on moq-relay's
`mod tests` only hits on Unix, because the host-clock tests are `cfg(unix)`.
`build.warnings=deny` makes the unfulfilled expectation an error. Scope the
`expect` to the Unix-only tests, or use `cfg_attr(unix, expect(...))`.

In the same PR, gate or remove the two moq-video items that are dead on
Windows (`pump.rs` `Read::Idle` and `from_bgra`). They warn today and will
fail the build the same way.

Planned 2026-10-25 (maintainer) from the triage of Nightly run
[40192013303](https://github.com/moq-dev/moq/actions/runs/40192013303).

Public API: none. Wire: none.
