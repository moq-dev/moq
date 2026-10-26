# [M] Relay demand gaps left by the widened-age re-offer

## Goal

Three gaps found by [moq-dev/moq#6717](https://github.com/moq-dev/moq/pull/6717)
are fixed or ruled out, each with a test:

- On lite before 07, a reader with a narrow max age receives the whole backlog.
- On moq-transport, a newcomer on a warm relay gets only the newest group.
- A relay newcomer may pull one group upstream twice: once from its range fill
  and once from the publisher's re-offer.

## Plan

Reproduce each through a relay first. The third is wasted upstream bandwidth
per newcomer, so it scales with subscribers; cover it with a benchmark if it
reproduces.

Public API: none expected. Wire: none expected.
