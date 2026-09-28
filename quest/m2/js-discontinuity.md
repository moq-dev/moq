# [XS] JS discontinuity() writes no estimated end

## Goal

`@moq/hang`'s `discontinuity()` without an explicit end closes the group with
no cadence-estimated end, as Rust `moq_mux::container::Producer::discontinuity`
does. Whatever resumes can land sooner than one estimated frame later (a
capture swap), and an end past it reads as a rewind to every consumer.

## Plan

The rename landed on `dev` in #4141 and kept the old behavior: in
`js/hang/src/container/legacy.ts`, `discontinuity(end?)` calls `#close(end)`,
which fills a missing `end` with `#end + #interval`. Rust's `discontinuity()`
calls `close(None, None)` and clears the cadence first.

Give the break its own close path that passes no estimate when the caller
gave no end, leaving the routine close's estimate alone. Test that a
discontinuity after a steady cadence writes no end past the last frame.

Public API: behavior change in `@moq/hang`'s `discontinuity()`, which exists
only on `dev`, so it targets `dev`. Wire: none.
