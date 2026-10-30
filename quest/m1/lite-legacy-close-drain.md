# [S] A lite-01..06 graceful close delivers the final group

## Goal

A publisher's graceful close on lite-01..06 never cuts a final group the
subscriber has not read yet, over WebTransport and QMux, in Rust and JS.

## Plan

Found while landing [moq-dev/moq#6969](https://github.com/moq-dev/moq/pull/6969):
the JS publisher's drain waits only for the transport's FIN
acknowledgement. The mock resolves that once the bytes are queued at the
peer, and its close discards unread chunks; WebTransport likewise errors a
session's receive streams on close, dropping their unread bytes. lite-07's
subscriber FIN avoids this.

Decided 2026-10-28 (maintainer): m1, since lite-01..06 are published and a
graceful close losing data breaks what "graceful" promises.

Reproduce first, over real WebTransport and QMux, for the JS publisher and
the Rust publisher. Then make the drain wait until the subscriber has read
the final group, using a signal lite-01..06 already carries (such as the
subscriber closing its side of the stream). If those versions carry no such
signal, stop and report the options rather than add a wait on a timer.

Public API: none expected. Wire: none; published versions keep their wire.
