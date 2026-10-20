# [XS] The interop runner withdraws publications before disconnecting

## Goal

Condition: a released moq-tokio and `@moq/net` carry the session `close()`
that withdraws this session's announcements before disconnecting. Then the
moq.dev clients in englishm/moq-interop-runner (`builds/moq-dev-rs`,
`builds/moq-dev-js`) bump to that release and end each publishing session
with `close()` instead of aborting.

Check: read the moq-tokio and `@moq/net` changelogs for a release with the
withdrawing `close()`. Delete this quest once the runner's moq.dev clients use that release's
`close()`.

## Plan

The maintainer approved changing the runner on 2026-10-19.
[englishm/moq-interop-runner#167](https://github.com/englishm/moq-interop-runner/pull/167)
moved both clients to the latest releases (moq-tokio 0.19.28, `@moq/net`
0.4.4) and kept their teardown, because 0.19.28's `close()` does not withdraw
announcements and adds about 500 ms per session against a distant relay,
which pushed `announce-subscribe` past its 3 s budget.

The stale namespace that motivated this ([#4209](https://github.com/moq-dev/moq/pull/4209):
a publisher that aborts without PUBLISH_NAMESPACE_DONE blocks the next run's
publish) no longer reproduces against imquic or any other public relay as of
2026-10-19, so this is cleanup rather than a fix. Re-measure `close()`'s cost
against imquic before switching, and keep each test inside its budget.
