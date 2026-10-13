# [S] Interop close-code test sees the refusal it expects

## Goal

The Interop browser close-code test (`test/interop/clients/js/close.ts`)
reliably reports a refused session as `unauthorized`, never as
"Connection lost", fixed at its cause.

## Plan

Seen 2026-10-10 on #5158's first Interop run (`refused session`: "Connection
lost" instead of "unauthorized"); it passed on rerun and the PR didn't touch
that code. Planned under this questline's cause-first rules (decided
2026-10-10): find whether the refusal's close code is lost on the wire (a
close frame racing the connection teardown, as in the qmux close-flush work)
or the client reads the connection's state before the close arrives. Fix
that at its source, with a regression that fails without it. No retry and no
accepting either message.

Investigation (2026-10-10): five independent Chromium refusals through an
isolated relay all resolved `WebTransport.closed` with code 2 and reason
`unauthorized`. The browser logged the same remote session error during SETUP.
The full Rust/Python/Go/C++/JS/C/GStreamer interop matrices for catalog warmup
and captions also passed the existing exact refusal assertion. No failing
observation was retried or accepted as an alternate result.

The harness reads the raw WebTransport close result rather than the client
library's error translation. A mocked-clock regression now proves one client
failure: when a SETUP operation rejects with `source: "session"`, the existing
100 ms reset-only fallback closes locally before a delayed authoritative
session close arrives. The draft reuses `sessionCause` to await that guaranteed
session close first. A remote unauthorized code survives a one-second mocked
delay; the regression fails without the change. Stream-only failures retain
the existing cleanup behavior, with no timeout increase or retry.

This does not establish that the historical Chromium `Connection lost`
observation had the same cause. The server HTTP/3 control-stream lifetime is
still another candidate. Preserve this quest until a failing browser trace
paired with relay logs establishes that cause and its regression.

Seen again 2026-10-11 on #5310's Interop run
[38121507310](https://github.com/moq-dev/moq/actions/runs/38121507310), whose
`interop-failure` artifact keeps the run directory and browser traces. The
relay log pins the refusal at admission, before the relay answered SETUP:
connection 64 closed with "the auth server refused the session"
(07:50:07.583), through `Connection::run`'s reject path, and the harness
reported "Connection lost" by 07:50:07.79. Pair this relay log with the
artifact's browser trace to tell whether the close capsule raced the
server's HTTP/3 control stream or connection teardown. #5310's next push
passed the same check.

Public API: none expected. Wire: none expected.
