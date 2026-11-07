# [S] Native WebSocketStream keeps datagrams flowing

## Goal

On Chromium's native `WebSocketStream`, a QMux session over WebSocket sheds a
datagram only when the socket has a real send backlog, not whenever any write
is in flight. A headless-Chromium test writes datagrams back to back on an idle
socket and proves every one arrives, and qmux is fixed if they don't.

## Plan

Planned 2026-11-07 (maintainer, /quest-plan of the follow-ups from the /quest-complete run) as the follow-up of
[moq-dev/moq#8054](https://github.com/moq-dev/moq/pull/8054). Decision prompts:

- ✅ Plan it, goal as stated
- Plan it, different goal
- Drop it

Milestone:

- ✅ m1
- m0
- m4

8054 found that the `WebSocketStream` ponyfill reported backpressure during
every write, because its writable used the default one-chunk high-water mark.
`WritableStreamSink.wantsMore()` in `js/qmux/src/scheduler.ts` sheds a datagram
as soon as `writer.desiredSize` reaches 0, so two datagrams written back to
back lost one. 8054 fixed only the ponyfill. When the platform has a native
`WebSocketStream` (Chromium), qmux uses it instead, and its writable may report
the same in-flight `desiredSize` of 0.

- Measure first: drive a QMux session over native `WebSocketStream` in
  headless Chromium, as the ponyfill's case in 8054's `test/datagram-tracks`
  does over Node, and record whether `desiredSize` drops to 0 while a write is
  in flight with nothing buffered.
- If it does, fix the native path in qmux so that only a real socket backlog
  sheds datagrams, and keep the test as the regression. If it doesn't, keep the
  test and finish.

Public API: none expected. Wire: none.

## Related

- [Datagram tracks](https://github.com/moq-dev/moq/pull/8054) - the PR whose ponyfill fix this extends to the native API
