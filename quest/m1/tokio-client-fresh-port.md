# [S] moq-tokio clients redial from a fresh source port

## Goal

A moq-tokio client whose reconnects keep failing rebinds to a fresh ephemeral
UDP port after N consecutive failed dials when its bind port is 0, so a
network fault that drops one 5-tuple can't strand it. Today every reconnect
reuses one socket, so with `--connect-backoff-timeout 0` a dropped flow
retries forever. A proxy test that drops one source port's packets shows the
client reconnecting on another.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1
- m2

[moq-dev/moq#7688](https://github.com/moq-dev/moq/pull/7688) (merged) fixed
this for the relay's cluster dials only.

- Reuse #7688's rebinding where it fits; choose N and record why.
- An explicit nonzero bind port is never changed.
- Model the test on #7688's proxy test, on controlled time.
- Every binding that takes `connect` settings gets the behavior through
  moq-tokio, with nothing new to configure unless the PR finds a reason.

Public API: none expected. Wire: none.
