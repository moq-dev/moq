# [XS] Publishing a draft opens its next changelog section

## Goal

After `just drafts publish NAME VERSION EMAIL` submits successfully, the
draft's changelog has an empty section for the next version above the one just
published, so entries written later land there instead of under a version the
datatracker already holds. Found by
[#5067](https://github.com/moq-dev/moq/pull/5067), which moved eight entries
that landed after moq-hang-03 was published out of its -03 section.

## Plan

Decided 2026-10-08:

- `sh/drafts/publish.sh` inserts the section itself on a 200 or 201 from the
  datatracker, in the draft's existing changelog heading style. The commit
  that records the publish carries it. Rejected: refusing to submit without a
  matching section (it checks the current version, not the next), and a
  written rule in `drafts/AGENTS.md` that nothing enforces.
- A failed submission leaves the file untouched.
- Ranked ahead of [Draft changelog audit](/quest/m2/drafts-changelog-audit.md),
  so the audit fixes the backlog once rather than repeatedly.
