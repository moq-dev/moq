# [S] Merge and publish the IETF close-code backport

## Goal

[moq-dev/moq#6959](https://github.com/moq-dev/moq/pull/6959) merges to
`release`, and a patch of `@moq/net` containing it publishes. Confirm the
published package includes the three protocol-violation close fixes from
[moq-dev/moq#6899](https://github.com/moq-dev/moq/pull/6899).

## Plan

The release-targeted implementation is prepared in #6959, with six regression
cases covering drafts 18 and 22: initial REQUEST_UPDATE, unsolicited
PUBLISH_NAMESPACE, and a track alias reused for different tracks.
The main-only advertisement budgets and SUBSCRIBE_TRACKS changes do not apply.

Decided 2026-10-27 (maintainer): PR creation only in this session; merging
and publication remain for a later authorized session. No package version was
bumped. Once the release PR has merged and its patch has published, delete
this quest and its links on main.

Public API: none. Wire: close codes only, matching main.
