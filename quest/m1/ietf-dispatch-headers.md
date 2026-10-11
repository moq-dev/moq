# [S] A slow IETF request header never blocks the next request

## Goal

`rs/moq-net`'s moq-transport session reads each incoming bidi request
stream's header in its own task, so a request stream whose header stalls
behind flow control never holds up accepting the next one.

## Plan

[#5086](https://github.com/moq-dev/moq/pull/5086) fixed the same pattern for
uni streams: `run_unis` waited for each stream's type before accepting the
next, and stalled under connection flow control. `run_dispatch`
(`rs/moq-net/src/ietf/session.rs`) still reads `id` and `ietf::Body` inside
the accept loop. Move the header read into the per-stream task, keeping the
current error handling: a stream that dies before its header is dropped, and
any other header error still fails the session. JS already handles each bidi
stream in its own task (`#runBidis` in `js/net/src/ietf/connection.ts`), so
this is Rust only (decided 2026-10-08).

Test, beside #5086's `a_silent_stream_does_not_hold_up_the_next`: a silent
bidi request stream does not block a SUBSCRIBE on the next one.

Review of [#5208](https://github.com/moq-dev/moq/pull/5208) found that
concurrent partial headers return transport credit before parsing completes,
allowing retention far beyond the receive window. Keep that PR held until
the shared native receive-memory work below bounds headers and frame data;
do not substitute a separate header quota or speculative-allocation fallback.

`release` has the same loop. Its backport needs a separate compatibility
assessment because the new receive-credit prerequisite targets the in-tree
stack; this plan does not authorize moving that stack onto `release`.

Public API: none. Wire: none.

## Required

- [Bound message assembly](/quest/m1/quic/receive-memory.md) - concurrent headers retain connection credit and pressure cancellation restores progress
