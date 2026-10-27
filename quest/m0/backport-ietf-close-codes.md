# [S] Backport the IETF close-code fixes to release

## Goal

`@moq/net` on `release` closes a moq-transport session with the right error for
the three cases [moq-dev/moq#6899](https://github.com/moq-dev/moq/pull/6899)
fixed on main, and publishes a patch release.

## Plan

Decided 2026-10-27 (maintainer): one PR against `release` per backport, in m0,
since these are hostile-input fixes in shipped packages.

The bugs on `release` (as of 6ce67943f7):

- `js/net/src/ietf/connection.ts:265`: a REQUEST_UPDATE as the initial message
  aborts only the stream instead of closing the session with a protocol violation.
- `connection.ts:314-317`: an unsolicited PUBLISH_NAMESPACE logs and closes
  cleanly instead of with an error.
- `subscriber.ts:807`: a `DuplicateTrackAlias` closes the session without its
  error code.

The announce-slot and SUBSCRIBE_TRACKS parts of #6899 are main-only and do not
apply. Port the regression tests that cover the three cases.

Public API: none. Wire: close codes only, matching main.
