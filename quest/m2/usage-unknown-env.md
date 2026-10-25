# [S] Upstream unknown-env option in usage-rs

## Goal

usage-rs ([jdx/usage](https://github.com/jdx/usage)) can refuse unknown
environment variables under a prefix, the way `unknown_flags = "error"`
refuses unknown flags. Once a release ships it, bump usage-rs and replace
`moq_tokio::cli::check_env` with the option. The option needs a way to
accept names read outside the spec, such as `moq_video::ENV`.

## Plan

Decided 2026-10-23 (maintainer interview): the maintainer approved posting
an issue or PR on jdx/usage for this. Waiting on jdx/usage is the condition:
check the upstream issue or PR and its releases; delete this quest if
upstream declines, keeping the helper.
