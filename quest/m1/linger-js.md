# [M] JS finishes a broadcast cleanly

## Goal

A `js/net` broadcast whose input ended cleanly stays servable until its
finished tracks' media is stale, then closes, matching Rust. `@moq/publish`
uses it when a publish ends cleanly (a source that stops), so a viewer still
catching up drains the tail. An interrupt ends the wait early.

## Plan

Decided (2026-10-16), mirroring the Rust library:

- `finish()` on the broadcast producer returns a finish handle, as
  `broadcast::Producer::finish` returns `broadcast::Finish` in Rust. The
  handle holds the finished tracks and keeps the broadcast announced; it
  exposes the `linger`, a `wait()` that resolves after it and closes, and a
  way to learn the broadcast closed elsewhere. Ending it early is the
  interrupt and closes at once. Rejected: a linger helper that each
  publisher holds and sleeps on, because every publisher would repeat the
  hold, and an origin-held linger, because it ties the session to what the
  origin holds.
- Tracks without a max-age add nothing to the wait. The alternative,
  `cache_duration`, defaults to `MAX` and would linger forever.

JS has no drop, so the handle needs an explicit interrupt: recommended, a
`close()` on the handle standing in for Rust's drop; an `AbortSignal` passed
to `wait()` is the other option. Report the API shape before landing.

Decide in `@moq/publish` which ends are clean (the source stops) and which
are abrupt (the element is removed, the user disables publishing).

Test with a mocked clock, as the Rust tests do: a late subscriber within
max-age gets the media, and the broadcast closes once it passes.
