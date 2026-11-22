# [S] A WHIP publisher past its cap is closed every run

## Goal

`moq-webrtc`'s `tests::a_whip_publisher_past_its_cap_plus_burst_is_closed`
passes every run, fixed at its cause.

## Plan

It fails about one run in four or six, run alone through `quest gate` on
`main` as well as on [moq-dev/moq#9371](https://github.com/moq-dev/moq/pull/9371)
(found 2026-11-22). A failing run hits the test's own 10s `TIMEOUT` waiting for
the session to end: "the publisher past its cap stayed open".

Suspected cause, to confirm first: the test pushes its one 1000-byte Opus frame
(`push(&mut *opus, 20_000, ...)`) before `push_whip` connects. Whenever the WHIP
client's subscription joins after that frame, nothing ever passes the cap. The
likely fix is to keep sending frames until the session ends, so the test
asserts the cap, not the join's timing.

Public API: none. Wire: none.
