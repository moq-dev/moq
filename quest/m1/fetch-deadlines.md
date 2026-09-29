# [S] moq fetch separates setup and read deadlines

## Goal

`moq fetch`'s timeout bounds reading the requested frames, not the connect, TLS, announce, and subscribe setup before them, so a slow setup can't eat the whole budget before the first frame is written. `fetch::tests::a_frame_read_times_out` stops failing under machine load.

## Plan

`rs/moq-cli/src/fetch.rs` wraps the whole run in one `timeout_at(deadline, ...)`, so the test's 500 ms budget covers setup plus the read. Under load setup alone can use it, and the test then fails for the wrong reason. Give setup and the read separate deadlines (or start the read deadline once the subscription is live), and assert the read timeout specifically. The test runs real sockets against an in-process relay, so it stays on the wall clock; mocked time would fire QUIC timers while packets are in flight. Found while fixing the moq-uring flakes (#4431).

Public API: none; check `moq fetch --help` and `doc/bin/cli.md` if a flag changes. Wire: none.
