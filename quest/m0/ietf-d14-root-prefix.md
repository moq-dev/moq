# [XS] No empty namespace prefix on draft-14

## Goal

On draft-14, moq-net never sends SUBSCRIBE_NAMESPACE with an empty prefix.
d14 §9.28 makes a prefix with N = 0 a PROTOCOL_VIOLATION, and moxygen
refuses it today when an unscoped moq-relay cluster link dials it. The
relay docs say an unscoped d14 link discovers nothing from moxygen, so
operators scope it.

## Plan

Triaged from Fastly's moq-relay-interop report (2026-09-23 run, item 2) on
2026-10-07.

Decided 2026-10-07: on d14, skip a SUBSCRIBE_NAMESPACE whose prefix is
empty. Keep today's warn-and-continue when the peer refuses a namespace
subscription. Rejected: failing the link on refusal, and requiring a scope
on d14 links at startup.

Revised 2026-10-07 after Fastly's pcap runs (#issuecomment-6046932019):
moxygen sends no PUBLISH_NAMESPACE on d14 to a peer that has not
subscribed, and moqx no longer speaks d14. So skipping alone leaves an
unscoped moq-dev downstream of moxygen with no namespaces. Decided to keep
the quest narrow and document the gap, since moxygen on d14 is the only
affected pair and scoped links already work. Rejected: subscribing per
prefix on demand when a local subscriber misses, and routing unknown
SUBSCRIBEs upstream; both need a hook on origin misses.

In flight as [#5018](https://github.com/moq-dev/moq/pull/5018), which also
covers draft-15 peers.

Where it lives: `subscribe_prefixes` (`rs/moq-net/src/ietf/subscriber.rs`)
returns `interest_prefixes(origin.allowed())`, which is `[""]` for an
unscoped origin. `run_subscribe_namespace` sends it. Log at debug when the root is skipped. The note belongs in
`doc/bin/relay/cluster.md`.

Test: a d14 session with an unscoped origin sends no SUBSCRIBE_NAMESPACE,
and a scoped one still does. Check `js/net` for the same path.

Public API: none. Wire: d14 stops sending a message the draft forbids.
