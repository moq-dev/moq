# [S] The GOAWAY drain deadline starts at the trigger

## Goal

A session's GOAWAY drain deadline counts from when the drain was triggered,
not from when the GOAWAY reached the wire, in moq-net (lite and IETF) and
js/net. A peer that stops reading can no longer stall the GOAWAY write and
hold the session open past its deadline.

## Plan

[moq-dev/moq#6144](https://github.com/moq-dev/moq/pull/6144) found that
moq-net arms its GOAWAY timer only after the message is written. A peer that
keeps the transport open but stops reading stalls that write forever, on lite
(open, flush, FIN ack) and IETF (`encode_message(..).await`). #6144 fixed the
relay only: its drain observer closes each session at the absolute deadline.
Library users of moq-net and js/net still hang.

- Arm the deadline at the trigger and race the write against it, so the
  relay's own deadline close becomes a backstop rather than the only bound.
- Port #6144's `a_stalled_goaway_write_is_still_closed_at_the_deadline` to
  moq-net and js/net, on a mocked clock, for lite and IETF.

Public API: behavior only. Wire: none.
