# [S] Rust and JS end a lite-05 track the same way after an inclusive SUBSCRIBE_END

## Goal

On lite-05, when a publisher from older `@moq/net` (0.1.3 to 0.1.9) sends
the inclusive SUBSCRIBE_END that `finish_at` refuses and the tail then
settles, Rust and JS subscribers agree on the outcome: both end the track
cleanly, or both only release it. A test in each language covers the
settled case.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295).
Milestone m3: a narrow legacy case, `@moq/net` 0.1.3 to 0.1.9 only.

[moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295) aligned the end signal (SUBSCRIBE_END on lite-05+, the FIN on
lite-01..04) and made a mid-tail session death abort in both languages. One
mismatch is left: with that inclusive SUBSCRIBE_END, Rust still ends the
track through the FIN (`ServeEnd::Finished` in
`rs/moq-net/src/lite/subscriber.rs`) once the tail settles, while JS only
releases it. Pick whichever matches what a current lite-05 publisher's
SUBSCRIBE_END produces, and change the other language. Extend [moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295)'s
`bare-fin` interop exchange (`test/interop/bare-fin.ts`, `just test
bare-fin`) with the inclusive encoding rather than the real-QUIC matrix,
for the reason [moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295) gives.

Public API: none. Wire: none; behavior only.

## Required

- [An ended lite track with an unsettled tail aborts at session death](/quest/m1/lite-track-end.md) - its PR, [moq-dev/moq#7295](https://github.com/moq-dev/moq/pull/7295), sets the end signal this finishes aligning
