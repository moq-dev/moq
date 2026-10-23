# [S] Parsers implement FromStr

## Goal

`hang`, `moq-auth`, and `moq-msf` parse through `impl FromStr` instead of an
inherent `pub fn from_str`, so `str::parse` works and the
`clippy::should_implement_trait` expectations go away.

## Plan

From the lint audit in
[moq-dev/moq#6159](https://github.com/moq-dev/moq/pull/6159), which kept the
inherent methods because the switch breaks callers. Decided 2026-10-21 with
the maintainer: do it in m2.
Moved to m1 on 2026-10-22 (maintainer, from an external API audit): the
cut is breaking anyway, so this break ships with it instead of forcing
another breaking release later. Today's inherent methods are in
`rs/hang/src/catalog/root.rs`, `rs/moq-auth/src/key.rs`,
`rs/moq-auth/src/set.rs`, and `rs/moq-msf/src/lib.rs`; check for more.
Remove the inherent method rather than keeping it beside the trait.

Public API: breaking for each crate, with a `MIGRATE.md` entry in each. Wire:
none.
