# [S] moqsrc waits for its session to end on stop

## Goal

`moqsrc`'s PAUSED to READY transition returns only after its session task and
connection have ended, as `moqsink` does since
[#4074](https://github.com/moq-dev/moq/pull/4074). Today
`SessionController::stop` in `rs/moq-gst/src/source/imp.rs` signals shutdown
and spawns a task to await the join, so an application that sets NULL and
exits right away can race aws-lc's exit destructors against an in-flight
dial, the silent SIGABRT the sink fix closed.

## Plan

Blocking needs care: the session task pushes buffers into src pads
(`block_in_place(|| pad.push(buffer))`), and a push blocked downstream while
`stop` waits on the task is a deadlock. Unblock the pads first (set them
flushing, or otherwise make a pending push return) before waiting, the way
GStreamer sources normally stop their streaming threads.

Reuse the sink's wait: it parks the thread on a waker instead of entering an
executor, and uses `block_in_place` when called from a multi-thread runtime
worker. Consider sharing it between the two elements rather than copying.

Tests mirroring the sink's: stop returns after the connection closes, stop
from a runtime worker does not deadlock, stop inside another executor does
not panic, and stop while a push is blocked downstream returns.
