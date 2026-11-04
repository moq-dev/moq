# [S] The draft-22 wider-fill refusal test is deterministic

## Goal

`draft-22: a wider fill is refused in %s order` in
`js/net/src/ietf/publisher.test.ts` passes reliably in both ascending and
descending order, alone and in the loaded suite. Find and fix why its live
group intermittently resets while being read, then remove its quarantine
without weakening the refusal or live-delivery assertions.

## Plan

Planned 2026-11-04 under the maintainer's rule to quarantine a known flaky
test immediately and plan its investigation. This is one fixture's flake,
so it is one child of this epic; m0 follows the epic's standing rule that a
flake fails an entire merge batch.

- The unchanged-main reproduction recorded in
  [moq-dev/moq#7860](https://github.com/moq-dev/moq/pull/7860) failed 30/200
  runs. [moq-dev/moq#7851](https://github.com/moq-dev/moq/pull/7851) already
  quarantines the same named case. The socket work in
  [moq-dev/moq#7883](https://github.com/moq-dev/moq/pull/7883) carries the
  precise investigation reference; its absolute-start adapter correction
  and controlled early-burst tests do not investigate this separate flake.
- Start from the recorded reproduction and trace the live group's reset in
  both orders. Establish whether fixture ownership, clock advancement,
  stream ordering, or production behavior causes it before choosing a fix.
  Control the responsible clock or event when the fixture is at fault;
  fix the production source when it is not.
- Restore the existing test in both orders with its assertions intact.
  Neither retries, longer timeouts, nor disabling production behavior is a
  fix. Record the original reproduction and loaded-suite results before
  and after, including the commands and limits of that evidence.

Public API: none expected. Wire: none.
