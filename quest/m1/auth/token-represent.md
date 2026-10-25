# [M] A lost AUTH stream re-presents its token

## Goal

When a session's AUTH stream ends without a verdict, the shared auth handle
presents its token again on the same session, with backoff, on lite and
moq-transport. The session never runs on without a token it was given.

## Plan

Since [moq-dev/moq#6109](https://github.com/moq-dev/moq/pull/6109), a token
whose AUTH stream is lost waits for the next session. The moq-transport
presenter has no retry loop at all. Retry on the live session with the
transient-refusal backoff (1s doubling to 30s), stop on a verdict or session
end, and test both wires with mocked time.

Public API: none expected. Wire: none.
