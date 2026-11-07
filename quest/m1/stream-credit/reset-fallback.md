# [S] A waiting higher-priority open resets the lowest-priority stream

## Goal

When the head of a session's open line waits for stream credit and a
lower-priority group stream is open, the session resets the lowest-priority
one, with at most one reset in flight per waiting open, so a slow
MAX_STREAMS never cuts more streams than needed. The reset group is reported
dropped on that subscription until [takeover](/quest/m1/stream-credit/takeover.md)
defines where it resumes. Every version and transport does this, browsers
included.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): split from "A higher-priority group makes room". Cancel is the
fallback the 2026-11-04 epic decision kept, and it needs no wire change, so
it lands before takeover. The trigger is the open line's waiting state
([moq-dev/moq#7894](https://github.com/moq-dev/moq/pull/7894)). A browser's
WebTransport reports no credit and keeps `createUnidirectionalStream()`
pending, so there JS acts once an open has waited about one round trip: the
RTT from WebTransport's `getStats()` when it reports one, else `Probe.rtt`
(as `js/net/src/connection/stats.ts` already falls back), and before any
sample the maintainer's original 100 ms.

Choose from the session's open group streams: Rust's
`transport::open::Lines::open_streams` (opened through
`boxed::Session::poll_open_group`) and JS's `lines(quic).streams`, each lowest
ranked first with whether the peer has acknowledged it. They report rank and
acknowledgement only; add the handle that reaches the stream's writer here.

Tests: a cap with no acknowledged stream resets exactly one stream per
waiting open, and the higher-priority group starts once credit returns.

Public API: none. Wire: none.
