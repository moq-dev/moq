# [M] A reuseport group is complete before it serves and keeps every socket

## Goal

`moq_sock::shard::Group` hands out no usable socket until every declared
member has bound and the steering filter is attached, and it retains every
socket for the served group's lifetime, so partial formation and an early
socket drop are unrepresentable rather than documented.

## Plan

Today the rule is left to the caller (rs/moq-sock/src/shard.rs:141-153): the
kernel numbers a reuseport group by what is in it, so closing one socket
renumbers every member after it and the filter steers their traffic to the
wrong sockets. `Member::bind` (:252-273) enforces bind order and hands the
socket straight back, and the filter goes on only when the last member binds
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
