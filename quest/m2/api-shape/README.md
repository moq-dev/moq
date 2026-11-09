# [L] Small API fixes before the cut

## Goal

The surfaces `main` adds since `release` follow the repository's API rules
before the cut publishes them: one path and one spelling per item, names by
role, no dead or unreachable public items, no setter that a caller can apply
to the wrong role, and Rust, JS, and the bindings mirrored. Each child is a
batch of small, mostly breaking fixes to one area, sized so one PR reviews in
one sitting.

## Plan

Planned 2026-11-07 (maintainer, /quest-plan) from an audit of `release`
(a005c7ad9a) against `main` (a9d7925123) plus the open quests. Nine
read-only reviewers (net model, net session, transport, media formats,
native media, apps, JS core, browser media, bindings) scored 127 clusters
for performance, usability, testability, and future fit, using
`just api-diff` and consumer searches in this repository and its
downstream applications. The maintainer approved the plan as shown. The larger findings
became their own m1, m2, m3, and m4 quests; this epic holds the rest.

This audit ran while [moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088),
the previous API audit's final pass, was open, and none of its children
repeats an item #8088 changes. #8088 has since merged, so a child rechecks
each item against `main` and drops what is already done.

Added 2026-11-07 (maintainer, /quest-plan from the 2026-11-07 API sweep of open PRs and remaining quests): fences for
what later quests grow (`#[non_exhaustive]` on structs and enums m3 to m5
add to), shapes those quests would otherwise break, and this epic's own
work: before the cut, run a "public item used only by its definition"
search over the published crates and JS packages and delete each hit, starting with
`moq_tokio::Server::serve_both`, `hang::Catalog::to_json_pretty`, and
`moq_audio::Format::{is_planar, is_float}`, the shortlist of
[the dead code audit](/quest/m3/audit-dead-code.md). The audit itself stays
in m3 for private code.

Decided 2026-11-09 (maintainer, on [moq-dev/moq#8302](https://github.com/moq-dev/moq/pull/8302):
"delete anything unused in general"): ✅ unused public surface is deleted,
not made private, and the pre-cut sweep covers the JS packages too
(rejected: a separate audit quest, since this epic already owns the sweep).

Rules every child follows:

- Removal beats addition. A public item with no caller in this repository
  or a known downstream application is deleted, unless an m1 or m2 quest plans a
  caller; it goes private only when internal code still uses it.
- No serde aliases, refusals, or shims for a spelling `release` never
  shipped.
- Each child lists its public API and wire impact in its PR, and its Impact
  section says how to migrate.

Rejected in planning:

- One audit PR for every area: each child touches a different owner's code,
  and #8088 showed that a 100-file audit PR spends weeks rebasing.
- Folding these items into the larger quests: those wait on decisions or
  prototypes, and these do not.

## Required

- [Ban doc(hidden) in Rust](/quest/m2/api-shape/doc-hidden.md) - `just check` bans it; each use becomes private or documented, test-support modules documented behind their feature
- [A peer's closed session reads as Closed in the bindings](/quest/m2/api-shape/session-closed.md) - a remote SESSION_CLOSED maps to `MoqError::Closed`, as a local close does
