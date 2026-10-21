# [S] A connection that never heard its peer closes at once

## Goal

moq-quic skips the close drain for a connection that never received a packet
from its peer, so closing a dial to an unreachable host returns at once
instead of waiting three PTOs (3 s at the default 333 ms initial RTT).
Stopping a `moqsink` whose relay is down stops promptly.

## Plan

Seen as moqsink's 3 s stop against an unreachable relay in
[moq-dev/moq#6152](https://github.com/moq-dev/moq/pull/6152), and as about ten
moq-gst tests paying 3 s each in
[moq-dev/moq#6174](https://github.com/moq-dev/moq/pull/6174). With no packet
from the peer, nothing on the other side needs the drain's CONNECTION_CLOSE
retransmits; a peer heard even once keeps the normal drain.

Open decision, public API, settled in this quest's PR with options and a
recommendation: should the configured idle timeout also bound the handshake?
Today moq-quic's handshake idle timeout is a fixed 10 s that neither
`--quic-idle-timeout` nor moq-uring's `Transport` sets, so three moq-uring
endpoint tests wait it out (#6174).

Regression tests: close a client whose dial never got an answer and assert
it returns without the drain, on a mocked clock.

Wire: none.

## Related

- [Socket close](/quest/m2/quic-socket-close.md) - releasing the endpoint's socket on close
- [Keep-alive from the idle timeout](/quest/m2/quic-keep-alive.md) - the other timer derived from the idle timeout
