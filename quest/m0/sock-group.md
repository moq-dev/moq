# [M] A reuseport group is complete before it serves and keeps every socket

## Goal

`moq_sock::shard::Group` hands out no usable socket until every declared
member has bound and the steering filter is attached, and it retains every
socket for the served group's lifetime, so partial formation and an early
socket drop are unrepresentable rather than documented.

## Plan

Today the rule is left to the caller (rs/moq-sock/src/shard.rs:141-153): the
kernel numbers a reuseport group by what is in it, so closing one socket
moves the last socket into the removed slot and invalidates existing
connection-ID steering for that socket. `Member::bind` (:252-273) enforces
bind order and hands the socket straight back, and the filter goes on only when the last member binds
(:344-349), so:

- binding fewer than all declared members exposes usable sockets before the
  steering filter exists;
- dropping an earlier socket before the last bind invalidates the recorded
  slot positions, and nothing notices.

Both current runtime callers bind every member before returning the group,
which is why this has not bitten. It is still a hole in the public API.

- Require complete formation before serving: a `Member::bind` yields a
  claim, not a socket, and the group releases sockets only once every member
  is in and the filter is attached.
- Retain every socket for the served group's lifetime, so a caller cannot
  close one without closing the group.
- Regressions for partial formation (no socket usable before the last bind)
  and for an early drop (refused, or the whole group ends).

The runtime-side consumer of this shape is
[#2964](/quest/m2/2964-quic-workers-dropping-one-split-server-resizes-the.md).

Keep `moq_tokio::bind` and its current names. The re-export is useful to
existing relay callers and does not require exposing the group's formation
state. Migrate affected in-tree consumers in the API change so they compile;
[#2964](/quest/m2/2964-quic-workers-dropping-one-split-server-resizes-the.md)
owns any remaining replacement of tokio's redundant internal bookkeeping.

Public API: breaking completed-member ownership in moq-sock 0.0.1. Preserve
the published moq-tokio worker signatures. Wire: no format change. Correct
the socket-removal explanation in source comments and docs inline, and run
the formation and drop regressions on Linux CI.
