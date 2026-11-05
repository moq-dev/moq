# [S] The P2P data-channel test transfers its frame

## Goal

Interop's P2P test (`test/p2p/transport.browser.ts`, two browser peer
connections completing MoQ SETUP and transferring a frame without a relay)
passes in scheduled Interop runs. Find the root cause and fix it, never by
raising the test's timeout or adding a retry.

## Plan

Added 2026-11-05 (maintainer, /quest-audit), from scheduled Interop
[run 42062226837](https://github.com/moq-dev/moq/actions/runs/42062226837/job/127366882573).
`window.checkTransport()` never returned, so the test timed out after
60000 ms at `test/p2p/transport.browser.ts:46`. Chromium logged one data
channel error just before:

```text
ERROR:third_party/blink/renderer/modules/peerconnection/rtc_data_channel.cc:867] Send failedINVALID_STATE
```

A send on a data channel that was not open (or already closing) failed, and
the transfer then stalled instead of failing. It timed out once before, in
[41668668824](https://github.com/moq-dev/moq/actions/runs/41668668824), and
passed locally then.

Public API: none expected. Wire: none.
