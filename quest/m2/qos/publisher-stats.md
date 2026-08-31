# [M] Publisher-reported media stats

## Goal

A publisher reports its own media stats in a catalog section a monitoring
consumer can read: bitrate, frame cadence, and what the publisher thinks it is
sending.

## Plan

Implement [#2734](https://github.com/moq-dev/moq/issues/2734): a
publisher-reported stats track carried as a catalog stats section. This is the
MEDIA half of a broadcast health verdict, bitrate, frame cadence, and what the
publisher thinks it is sending, as opposed to the transport backlog.

Version the section so older publishers remain usable without fabricating
media health. Reject malformed or implausible samples, classify missing or
stale telemetry as unknown, and treat the self-report as diagnostics only,
never as billing, authorization, or route-selection input.

Land the catalog section, publisher API, fixtures, and tests. No PR exists
yet, so writing the implementation is this quest's work and nothing external
gates a start. The moq.pro (downstream) health badge consumes the released
section downstream.

Independent of [backlog](/quest/m2/qos/backlog.md): the two halves land in
either order, and a health verdict needs both. Keep connection-scoped sender
transport in its own quest because it has a different wire owner and consumer.

## Closes

- [#2734](https://github.com/moq-dev/moq/issues/2734) - close this issue when the quest finishes
