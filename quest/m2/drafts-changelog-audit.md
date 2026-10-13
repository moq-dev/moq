# [XS] Every draft's changelog matches what it published

## Goal

For each draft in `drafts/` with a published version, the changelog section
for its newest published version lists only what that published text
contains. Entries that landed after publication move to the next version's
section. This corrects attribution, not any format.

## Plan

Decided 2026-10-08, after [#5067](https://github.com/moq-dev/moq/pull/5067)
found eight late entries under moq-hang-03:

- Scope is every published draft, not only moq-lite. Find each one's newest
  version on the datatracker and compare every entry in that section against
  the published body and changelog, as #5067 did for hang; do not rely on an
  illustrative list.
- Skip drafts that have never been published. Run `just drafts check`.
