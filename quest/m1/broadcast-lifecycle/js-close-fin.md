# [S] js/net finishes tracks whose FIN arrives with the close

## Goal

When a peer closes a session gracefully right after finishing its tracks, a
js/net relay or subscriber finishes those tracks instead of aborting them, on
every lite version, as Rust does since
[moq-dev/moq#6741](https://github.com/moq-dev/moq/pull/6741).

## Plan

In Rust the close could reach the subscriber before it read an already
buffered SUBSCRIBE FIN, so the track ended `Unroutable`. The fix polls the
sources once after the closed transport's group streams drain. Check whether
js/net has the same ordering; if so, reproduce with a two-`Connection` test and
mirror the fix. If not, delete this quest with the evidence.

Public API: none. Wire: none.
