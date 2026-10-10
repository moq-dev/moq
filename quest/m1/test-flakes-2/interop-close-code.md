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
library's error translation. `Connection.connectTransport` still has a
100 ms SETUP-error close grace, after which it locally closes the transport.
Whether that competes with a delayed remote capsule under load is unproven;
the server HTTP/3 control-stream lifetime is another candidate. A failing
browser trace paired with relay logs is needed before altering either
lifetime. No timeout, retry, or accepted error was changed.

Public API: none expected. Wire: none expected.
