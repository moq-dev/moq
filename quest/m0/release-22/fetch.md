# [S] Draft-20+ FETCH on the 0.17 release line

## Goal

A 0.17.x moq-relay serves single-group FETCH on draft-20 and later sessions,
instead of refusing with "FETCH not supported on draft-20" on draft-22.

## Plan

Reported from draft-22 interop on 2026-10-12 against moq-relay 0.17.3.
`main` already serves it ([#4971](https://github.com/moq-dev/moq/pull/4971),
"serve draft-20 FETCH within one group"); `release` still refuses on
`Filter::is_draft20(version)`.

Decided 2026-10-12: backport #4971 onto `release` as its own PR
(CONTRIBUTING), including its tests, then cut the next 0.17 patch with the
release-plz PR. Mirror the `@moq/net` side if #4971 touched `js/net`.
