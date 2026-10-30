# [S] A lite-01..04 graceful close delivers the final group

## Goal

A publisher's graceful close on lite-01..04 never cuts a final group the
subscriber has not read yet, over WebTransport and QMux, in Rust and JS.

## Plan

Found while landing [moq-dev/moq#6969](https://github.com/moq-dev/moq/pull/6969):
the publisher's drain waits only for the transport's FIN acknowledgement,
and a receiver may drop unread bytes on close (WebTransport errors a
session's receive streams on close, and the JS mock discards unread chunks).

Decided 2026-10-28 (maintainer): m1, since lite-01..06 are published and a
graceful close losing data breaks what "graceful" promises.

[moq-dev/moq#7296](https://github.com/moq-dev/moq/pull/7296) fixed lite-05
and lite-06: their publishers now wait for the subscriber's FIN, which Rust
and JS subscribers send once their tail settles, as lite-07 already did.
SUBSCRIBE_END says which groups are owed, so that FIN is prompt.

lite-01..04 carry no SUBSCRIBE_END, so a subscriber settles its tail only
when its grace expires (the subscription's max delay, or one second), and
only then FINs. Waiting for that FIN is a wait on the subscriber's timer: a
close takes at least the grace and, with the defaults, hits the one-second
drain cap and reports a timeout. The maintainer picks one of:

- Keep the ACK drain on lite-01..04 and document that only lite-05+ closes
  wait for the subscriber to read the tail. Delete this quest.
- Wait for the subscriber's FIN on lite-01..04 too, accepting a close that
  waits out the grace and usually times out.

Public API: none expected. Wire: none; published versions keep their wire.
