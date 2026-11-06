# [S] Upstream unknown-env option in usage-rs

## Goal

usage-rs ([jdx/usage](https://github.com/jdx/usage)) can refuse unknown
environment variables under a prefix, the way `unknown_flags = "error"`
refuses unknown flags. Once a release ships it, bump usage-rs and replace
`moq_tokio::cli::check_env` with the option. The option needs a way to
accept names read outside the spec, such as `moq_video::ENV`.

## Plan

Filed 2026-11-02 (maintainer-approved, /quest-unblock). No existing issue or
release covered it; the closest, clap's declined `env_prefix`, builds names
from a prefix, which this does not.

Shipped 2026-11-03 (checked 2026-11-04, /quest-unblock): usage-lib
[7.2.0](https://github.com/jdx/usage/releases/tag/v7.2.0) adds the opt-in
`unknown_env` node ([jdx/usage#1609](https://github.com/jdx/usage/pull/1609),
closing jdx/usage#1607), with derive syntax
`#[usage(unknown_env(prefix = "MOQ_", allow(...)))]` for names read outside
the spec.
