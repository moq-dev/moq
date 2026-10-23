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

In m1 because it owns a red interop cell (decided 2026-10-22): `moq`
against moxygen on draft-14 discovers nothing. moq-net never asks draft-14
or 15 for the empty namespace, and moxygen announces nothing unasked. The
CLI does no scoping of its own: the subtree scope tried for this in
[moq-dev/moq#6255](https://github.com/moq-dev/moq/pull/6255) lost the
sibling broadcasts a catalog references, such as a transcode's `.` source.
Asking for the watched prefixes fixes the cell without that loss; verify
it with `MOQ_BIN=target/debug/moq just relays --only moxygen` from
`test/smoke`.

- First step: Rust sends UNSUBSCRIBE_NAMESPACE on d14/15. It never does
  today ([moq-dev/moq#6133](https://github.com/moq-dev/moq/pull/6133)).
- Then drive SUBSCRIBE_NAMESPACE from the origin's watched prefixes, as the
  lite path does, in Rust and JS.
- Decide what an unsolicited PUBLISH_NAMESPACE outside any watched prefix
  costs the receiver, and bound it.

Public API: none expected. Wire: no new messages.

## Related

- [Keyed prefix polling](/quest/m2/keyed-prefix-polling.md) - the per-wake cost of many open prefixes
