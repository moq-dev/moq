# [M] Each session knows its open streams and which are acknowledged

## Goal

Rust and `js/net` sessions keep a per-session list of their open outgoing
group streams with each stream's priority, and the transports that can tell
report when all of a stream's sent data is acknowledged: moq-quic (raw QUIC
and WebTransport), Rust qmux, `@moq/qmux`, and the memory transport. A
browser's WebTransport cannot report it, and the session knows that at setup
rather than guessing. This is what [reset fallback](/quest/m1/stream-credit/reset-fallback.md)
and [takeover](/quest/m1/stream-credit/takeover.md) choose a stream from.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): split from the [M] "A higher-priority group makes room" quest, which
the agent that declined it on 2026-11-05 found was really XL and had gaps:
the transport could not report when a stream's data is all acknowledged,
nothing defined where a reset group resumes, and no session listed its open
streams. This part starts now; it touches no code
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670) rewrites.

The signal that room is needed is the open line's waiting state (the head of
the line is pending), per the maintainer's 2026-11-04 decision on
[moq-dev/moq#7894](https://github.com/moq-dev/moq/pull/7894), not a transport
credit or blocked API. Acknowledgement reporting is a different fact (has the
peer received this stream's bytes), so it is new transport surface; keep it
to what takeover needs.

Public API: transport traits gain an acknowledged signal where supported.
Wire: none.
