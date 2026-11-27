# [L] Delete moq-pattern and @moq/pattern

## Goal

Remove the `moq-pattern` Rust crate and `@moq/pattern` JS package, folding
`Prefix`, `Prefixes`, and path-segment helpers (`parts`, `escape`, `unescape`)
into `moq-net` and `@moq/net` (re-exported where `moq-auth` and `@moq/auth`
need them), and remove wildcard `Pattern` / `Patterns` discovery filters and
`captures` across `moq-net`, `moq-auth`, `moq-relay`, and the JS packages.

## Plan

Decided 2026-11-27 (maintainer, `/quest-plan`):

- ✅ Delete `moq-pattern` and `@moq/pattern` completely (rejected: keep the
  wildcard pattern algebra for local discovery filters). Wildcards never
  transit the wire, grants already dropped them in
  [moq-dev/moq#9251](https://github.com/moq-dev/moq/pull/9251), and keeping
  `Pattern`, `Patterns`, and `captures` inside `moq-net`'s `origin.rs` adds
  significant complexity for a capability no first-party caller uses.
- ✅ Land in `m2` before the `0.22.0` cut (rejected: defer to `m3` after
  `0.22.0`; split into `m3` while removing catalog `instance` in `m2`).
  Cutting `0.22.0` with `moq-pattern` / `@moq/pattern` and `Consumer::filter`
  only to delete them in the next breaking cycle wastes a published surface.

Scope:

- Move `Prefix`, `Prefixes`, `InvalidPrefix`, and the segment escaping/splitting
  helpers (`parts`, `Parts`, `escape`, `unescape`) into `moq-net::path` and
  `@moq/net` (re-exported by `moq-auth` / `@moq/auth` as needed).
- Remove `Pattern`, `Patterns`, `Segment`, `Specificity`, `IntersectionError`,
  `InvalidPattern`, `Consumer::filter(&Pattern)`, `Announce.captures`,
  `TableCursor::captures`, `interest_prefixes(&Patterns)`, `prefix_claim`, and
  `Mount::translate(&Patterns)` from `moq-net` and `@moq/net`. Keep prefix
  scoping (`scope`) plus exact-path lookup for `path_routes`; callers wanting
  ad-hoc client-side filtering filter announced paths directly.
- Change `stats::Config::{exclude, exclude_rooted}` from `Patterns` to
  `Prefixes`, and `moq-relay`'s `SessionFilter.path` from `Option<Pattern>` to
  `Option<Prefix>`.
- Delete `rs/moq-pattern` and `js/pattern` along with workspace members,
  criterion benches (`sh/rs/instructions.sh`, `sh/gh/nightly.sh`), feature-flag
  lists, trusted-publisher lists, and docs references; update affected quests
  ([feature-flags](/quest/m3/release/feature-flags.md),
  [crates-trusted-publisher](/quest/m3/crates-trusted-publisher.md),
  [grant-alias-delete](/quest/m4/grant-alias-delete.md),
  [regrant-route-recheck](/quest/m4/regrant-route-recheck.md),
  [peer-grant-bound](/quest/m5/peer-grant-bound.md),
  [rs2ts-model-subset](/quest/m5/rs2ts-model-subset.md)) to reference `moq-net`
  / `@moq/net` prefixes.
- Keep `moq-auth`'s temporary `foo/**` grant read/write migration alias intact
  until [grant-alias-delete](/quest/m4/grant-alias-delete.md) removes it,
  implemented directly on `Prefix` without `moq-pattern`.

## Related

- [Remove instance from hang::catalog](/quest/m2/remove-catalog-instance.md) -
  companion pre-0.22 cleanup
- [Grants stop spelling prefixes as subtree patterns](/quest/m4/grant-alias-delete.md) -
  removes the temporary `foo/**` grant alias after 0.22
- [Bound peer grant processing by design](/quest/m5/peer-grant-bound.md) -
  benchmarks `Prefixes` set operations
