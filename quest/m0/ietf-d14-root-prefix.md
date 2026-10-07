# [XS] No empty namespace prefix on draft-14

## Goal

On draft-14, moq-net never sends SUBSCRIBE_NAMESPACE with an empty prefix.
d14 §9.28 makes a prefix with N = 0 a PROTOCOL_VIOLATION, and moxygen and
moqx refuse it today when an unscoped moq-relay cluster link dials them.

## Plan

Triaged from Fastly's moq-relay-interop report (2026-09-23 run, item 2) on
2026-10-07.

Decided 2026-10-07: on d14, skip a SUBSCRIBE_NAMESPACE whose prefix is
empty, and rely on the peer's unsolicited PUBLISH_NAMESPACE, which d14 §6.2
allows. Keep today's warn-and-continue when the peer refuses a namespace
subscription. Rejected: failing the link on refusal, and requiring a scope
on d14 links at startup.

Where it lives: `subscribe_prefixes` (`rs/moq-net/src/ietf/subscriber.rs`,
around line 600) returns `interest_prefixes(origin.allowed())`, which is
`[""]` for an unscoped origin. `run_subscribe_namespace` (around line 861)
sends it. Log at debug when the root is skipped.

Test: a d14 session with an unscoped origin sends no SUBSCRIBE_NAMESPACE,
and a scoped one still does. Check `js/net` for the same path.

Public API: none. Wire: d14 stops sending a message the draft forbids.
