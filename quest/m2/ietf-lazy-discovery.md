# [L] IETF lazy discovery

## Goal

A moq-transport session, like a lite one, asks for announcements only under
the prefixes its origin is watching, and withdraws the ask when nobody
watches any more, on every draft we support.

## Plan

[moq-dev/moq#6165](https://github.com/moq-dev/moq/pull/6165) made lite
discovery lazy and kept IETF eager: lazy SUBSCRIBE_NAMESPACE needs an
UNSUBSCRIBE_NAMESPACE path on drafts 14 and 15, and peers still push
unsolicited PUBLISH_NAMESPACE.

- First step: Rust sends UNSUBSCRIBE_NAMESPACE on d14/15. It never does
  today ([moq-dev/moq#6133](https://github.com/moq-dev/moq/pull/6133)).
- Then drive SUBSCRIBE_NAMESPACE from the origin's watched prefixes, as the
  lite path does, in Rust and JS.
- Decide what an unsolicited PUBLISH_NAMESPACE outside any watched prefix
  costs the receiver, and bound it.

Public API: none expected. Wire: no new messages.

## Related

- [Keyed prefix polling](/quest/m2/keyed-prefix-polling.md) - the per-wake cost of many open prefixes
