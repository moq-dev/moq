# Small API fixes before the cut

## Goal

The surfaces `main` adds since `release` follow the repository's API rules
before the cut publishes them. The epic's own pre-cut deletion sweep landed in
[moq-dev/moq#8461](https://github.com/moq-dev/moq/pull/8461); it finishes with
its remaining children.

## Plan

Rules every child follows:

- Removal beats addition. A public item with no caller in this repository
  or a known downstream application is deleted, unless an m1 or m2 quest plans a
  caller; it goes private only when internal code still uses it.
- No serde aliases, refusals, or shims for a spelling `release` never
  shipped.
- Each child lists its public API and wire impact in its PR, and its Impact
  section says how to migrate.
