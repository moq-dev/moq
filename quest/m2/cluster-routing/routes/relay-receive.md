# [L] A relay folds received routes incrementally

## Goal

A relay's work per route batch it receives scales with the claims that
changed, not with the size of the origin they belong to. A benchmark swept
over both axes (claims per origin, claims changed per batch) shows the cost
flat in origin size.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8293](https://github.com/moq-dev/moq/pull/8293)). Milestone: ✅
m2, a child of this epic (rejected: m3, dropping it). #8293 makes the origin
node and each stream's presentation incremental. The receive side still
does linear work per change:

- each received batch clones the heard view and merges the whole view
  (`View::merge`, `retire`);
- `node::State::home` walks every shown claim of the identity.

So one claim change on a large origin costs a pass over all of its claims at
every relay that hears it. Fold each batch into the held view by the claims
it names, and keep whatever `home` derives up to date per change instead of
recomputing it. #8293's benchmark covers only the origin; extend it to the
receiving relay, so both axes are swept per the repository's fan-out rule.

Public API: none expected. Wire: none.

## Required

- [Route session integration](/quest/m2/cluster-routing/routes/session.md) - the incremental origin and per-stream presentation this builds on
