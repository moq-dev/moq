# [S] Rust and JS end a lite-05 track the same way after an inclusive SUBSCRIBE_END

## Goal

On lite-05, when a publisher from older `@moq/net` (0.1.3 to 0.1.9) sends
the inclusive SUBSCRIBE_END that `finish_at` refuses and the tail then
settles, Rust and JS subscribers both end the track cleanly, as a current
lite-05 publisher's SUBSCRIBE_END does. A test in each language asserts that
clean end for the settled case.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295) (merged).
Milestone m3: a narrow legacy case, `@moq/net` 0.1.3 to 0.1.9 only.

[moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295) aligned the end signal (SUBSCRIBE_END on lite-05+, the FIN on
lite-01..04) and made a mid-tail session death abort in both languages. One
mismatch is left: with that inclusive SUBSCRIBE_END, Rust still ends the
track through the FIN (`ServeEnd::Finished` in
`rs/moq-net/src/lite/subscriber.rs`) once the tail settles, while JS only
releases it. Match what a current lite-05 publisher's SUBSCRIBE_END
produces, a clean end, so JS changes and Rust keeps its behavior. Extend [moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295)'s
`bare-fin` interop exchange (`test/interop/bare-fin.ts`, `just test
bare-fin`) with the inclusive encoding rather than the real-QUIC matrix,
for the reason [moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295) gives.

Public API: none. Wire: none; behavior only.
