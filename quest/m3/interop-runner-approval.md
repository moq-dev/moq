# [XS] The interop runner withdraws publications before disconnecting

## Goal

Condition: the maintainer approves changing moq-interop-runner
(englishm/moq-interop-runner, outside this repository), and a released
moq-tokio and `@moq/net` carry the session `close()` that withdraws this
session's announcements before disconnecting. Then post the one-line change:
a completed publish in the runner calls `close()` instead of aborting, so a
subsequent run can publish the same namespace without an already-published
error.

Check: ask the maintainer, or look for the approval on the runner's
repository, and read the moq-tokio and `@moq/net` changelogs for a release
with the withdrawing `close()`. Delete this quest once the change is posted.

## Plan

The runner found this in [#4209](https://github.com/moq-dev/moq/pull/4209).
Exercise successive runs against the same relay before posting.

Decided 2026-10-08: the graceful-close quest folds in here as one condition
quest, since the runner change is one line once both conditions hold.
