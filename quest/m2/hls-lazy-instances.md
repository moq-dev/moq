# [S] Retained HLS instances watch only when read

## Goal

A replaced HLS instance that the server keeps for readers following a
restart starts its catalog and timeline watchers only when a request reaches
it, so an idle retained instance costs no tasks.

## Plan

Decided with the maintainer on 2026-10-24: m2, an efficiency gain. Retention
is already bounded by the broadcasts the publisher keeps open
([moq-dev/moq#6649](https://github.com/moq-dev/moq/pull/6649)).

- Start the watchers on the first request for that instance, keep them
  while requests continue, stop them once its readers go idle, and restart
  them on a later request. Tests show an unread retained instance holds
  none, including after its last reader leaves, and that a later request
  restarts them.
