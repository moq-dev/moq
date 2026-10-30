# [XS] Flatten nested Options for the TS translation

## Goal

`resolve_fetch_group`, `request_start`, and `Live::base` in moq-net stop building an
`Option<Option<_>>`, so moq-dev/rust2's TypeScript translator can translate them.
Behavior is unchanged, and the code stays at least as clear.

## Plan

Decided with the maintainer on 2026-10-26, from moq-dev/rust2's
[census fixes](https://github.com/moq-dev/rust2/pull/78): rewrite these three
functions in moq rather than give the translator a second `Option`
representation. Use a named enum or an early return where it reads better, and keep
a change only if it does.

Public API: none.
