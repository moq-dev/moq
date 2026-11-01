# [S] Every moq-c consumer cancels while its subscription is pending

## Goal

Cancelling a hand-written moq-c catalog, raw track, datagram, or media
consumer ends it with a clean final callback even while it waits for its
subscription to be accepted, as the JSON consumers do since
[moq-dev/moq#7082](https://github.com/moq-dev/moq/pull/7082).

## Plan

Decided 2026-10-30 (maintainer, `/quest-plan`): the hand-written crate ships
at the cut and the [generated C line](/quest/m2/c/README.md) follows it, so
fix this bug for the release, but skip converting the consumer loops to kio
polling and changing the producers: the generated binding replaces both.

Reuse #7082's `Consume::json_subscribe` pattern for the other consumers, with
a test per consumer that cancels an unanswered subscription.
