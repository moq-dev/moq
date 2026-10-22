# [S] Interop covers a clean broadcast finish

## Goal

The interop matrix has a media "finish" case: a publisher in each
implementation finishes its broadcast cleanly, and every subscriber drains
the tail and then sees the broadcast end, rather than an abort. A regression
in any implementation's linger shows up as a red cell.

## Plan

Rust, JS ([moq-dev/moq#6151](https://github.com/moq-dev/moq/pull/6151)),
and the bindings ([moq-dev/moq#6157](https://github.com/moq-dev/moq/pull/6157))
each have a finish. Drive it from the existing
interop clients rather than adding a new harness, and assert both halves:
the late tail arrives and the end is clean. The new case's cells show up
in the nightly report at <https://git.moq.dev/interop/>.

Public API: none. Wire: none.

## Progress

The candidate in [moq-dev/moq#6307](https://github.com/moq-dev/moq/pull/6307)
adds finish-only modes to the existing Rust, browser, native JS, C, C++, Go,
and Python clients and `case=finish` cells in the existing nightly runner.
Preconnected readers begin media demand only after publication is finished;
ten nonempty frames, natural media/catalog EOF, exact announcement END, and a
new untimed health track on the same session are required. The publisher waits
its actual finish linger before health; a separate FIFO only releases teardown.

## Decisions needed

The goal includes GStreamer subscribers, whose current documented policy holds
pads across whole-broadcast finish/withdrawal for a later route. Its public
`moqsrc` interface does not expose the connection, natural media/catalog EOF,
or announcement END. Track errors also do not produce a distinct downstream
terminal event. The runner leaves those finish cells red rather than claiming
cancellation, a second health connection, or synthetic EOS proves the goal.
Choose explicit lifecycle observability while preserving persistent watch
(recommended), or a separately selected finite-watch policy. This needs a
maintainer API/behavior decision before completing the quest.
