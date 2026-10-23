# [S] Upstream unknown-env option in usage-rs

## Goal

usage-rs ([jdx/usage](https://github.com/jdx/usage)) can refuse unknown
environment variables under a prefix, the way `unknown_flags = "error"`
refuses unknown flags. Once a release ships it, bump usage-rs and replace
the `moq_tokio::cli` helper from [the env check](/quest/m1/env-unknown.md)
with the option.

## Plan

Decided 2026-10-23 (maintainer interview): the maintainer approved posting
an issue or PR on jdx/usage for this. Waiting on jdx/usage is the condition:
check the upstream issue or PR and its releases; delete this quest if
upstream declines, keeping the helper.

## Required

- [Refuse unknown MOQ_* environment variables](/quest/m1/env-unknown.md) - ships the helper this replaces
