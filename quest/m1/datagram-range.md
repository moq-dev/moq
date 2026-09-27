# [S] Datagram range

## Goal

A subscriber receives only the datagrams its subscription asked for, on
moq-lite and on moq-transport, in Rust and JavaScript. A new subscriber
is not handed datagrams from before its start.

## Plan

Datagrams share the group sequence namespace, but their cursor ignores the
subscription's start and end. The model buffers the last 64 per track, and a
new subscriber's cursor starts at the oldest. So a late joiner, or a relay
fanning out a fresh downstream, gets stale datagrams first. Both protocols do
this today, and the moxygen line kept moq-transport matching moq-lite.

Settle the rule once and apply it to both protocols. It could be the cursor
starting at the live edge, the subscribe range bounding datagrams the way it
bounds groups, or both. Prefer fixing it in the model over filtering in each
session.

Watch the edge cases: a datagram at the start group when a frame offset
skips object 0, SUBSCRIBE_UPDATE moving the range, and a datagram that
lands before the subscription's alias or id is known. A test must tell a
filtered datagram apart from one dropped for any other reason.

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - brought datagrams to moq-transport with moq-lite's behavior
