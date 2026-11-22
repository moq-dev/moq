# [M] An update racing a moq-transport run's end closes at most its subscription

## Goal

A SUBSCRIBE_UPDATE (REQUEST_UPDATE on draft 22) that crosses the end of its
subscription on a moq-transport session ends at most that subscription; the
session and its other broadcasts carry on. A test reproduces the race and
shows the session survives it.

## Plan

Decided 2026-11-22 (maintainer, /quest-plan): ✅ plan it (rejected: drop it
because nothing sends that update any more).

Found on [moq-dev/moq#9265](https://github.com/moq-dev/moq/pull/9265): moq-ffi's
`server_client_run_end` tests failed about 7 to 12% of the time when a relay
copy sent an update asking from group 0 for a moq-transport live join. The
update raced the end of the run and the session closed with
PROTOCOL_VIOLATION. #9265 stopped sending that update, so the tests pass, but
the close itself was never found. AGENTS.md requires a peer's mistake, or a
benign race, to cost no other broadcast, and a session close costs all of
them.

Where the session closes is unknown. On main, `ietf/publisher.rs`
discards an update that crossed PUBLISH_DONE (`poll_retain_join`), accepts a
retired request ID (`control.rs` `accept`), and turns an update to an ended
track into that subscription's PUBLISH_DONE. The remaining session-closing
paths an update reaches are a decode error (`Turn::Malformed`,
`UpdateRead::Malformed`) and an `accept` failure, and either side
(subscriber or publisher) may be the one closing. Start by reproducing it:
restore the from-zero update at #9265's earlier head, or send one in a test
right as the publisher finishes, under stress (the original rate was about 1 in
8 to 1 in 14), and log which close fires.

## Related

- [A moq-transport reader from the oldest group gets the backlog](/quest/m1/ietf-oldest-backlog.md) - sends a from-zero request again, so this race may resurface there
