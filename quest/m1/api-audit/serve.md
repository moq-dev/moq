# [M] One range setter

## Goal

A reader moves its cursor with one consuming `with_range(self, range)` (JS
`withRange`) on `track::Subscriber`, `track::Ordered`, `track::Subscription`,
and `group::Consumer`, replacing `set_groups`, `with_groups`, `start_at`,
`raise_start_to`, `end_at`, `subscription::Cap`, and
`group::Consumer::set_frames`. Nothing else becomes public.

## Plan

Decided 2026-10-23 (maintainer), reshaping
[moq-dev/moq#6138](https://github.com/moq-dev/moq/pull/6138) after the
public-model goal was dropped
([session-only items stay private](/quest/m1/api-audit/model-private.md)):

- Keep `with_range` and the bug fixes #6138 carries: js/json and js/flate
  `latest()` keeping the caller's cap, `group::Consumer::poll_finished`
  ending cleanly at a frame cap, and a closed JS subscriber's groups still
  expiring.
- Everything else #6138 made public for custom publishers stays internal:
  `track::Subscriber::{poll_start, poll_drop}`,
  `group::Consumer::poll_expired`, `Position::final_sequence`,
  `Subscription::{start, end, with_start, with_end}`, and the JS `Recv`
  union, `groupChanged`, `Ordered.tryNextGroup`, `finalSequence`,
  `Consumer.info({ hold })`, and `Group.Consumer.expired`. The sessions call
  them through `pub(crate)` and `@internal` (or the existing hooks).

Public API: moq-net and `@moq/net` breaking (`with_range` replaces the
setters). Wire: none.
