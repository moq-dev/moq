# [S] Unify dependency features with cargo-hakari

## Goal

The FFI builds compile each dependency with the same features as each other,
so mbx's cache hits across them, and Nightly's hosted platform jobs record
their hit rate and CPU time before and after the workspace-hack.

## Plan

Decided with the maintainer on 2026-10-25: use cargo-hakari, the standard tool
from guppy's authors, rather than only trimming CI's feature permutations. mbx
keys every compilation on its inputs, so a dependency built with a different
feature set in another job is a miss, however warm the cache.

[moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985) added
`rs/workspace-hack` as a path-only dev-dependency of every member outside
`.config/hakari.toml`'s excludes. Test, lint, and bench builds now unify;
library-only builds do not, since dev-dependencies stay out of them. That
leaves the FFI builds (`cargo build --release -p moq-ffi` in the bindings and
release workflows), which also split on their own flags: the Dart build passes
the host triple as `--target` and adds the `testing` feature. Align those
invocations rather than add the hack to them, unless the PR's decision on the
layout says otherwise.

Nightly's macOS and Windows jobs read and write R2, so their before and after
can only come from scheduled Nightlies on `main`
(`gh run list --workflow nightly.yml --branch main --event schedule`): compare
the mbx summaries of the last run before the merge with the first two after.

Public API: none; the workspace-hack crate is not published.
