# [M] JS finishes a broadcast cleanly

## Goal

A `js/net` broadcast whose input ended cleanly stays servable until its
finished tracks' media is stale, then closes, matching Rust. `@moq/publish`
uses it when a publish ends cleanly (a source that stops), so a viewer still
catching up drains the tail. An interrupt ends the wait early.

## Plan

Decided (2026-10-16), mirroring the Rust library:

- `finish()` on the broadcast producer returns something to await, next to
  `close()`, as `broadcast::Producer::finish` does in Rust. Ending the wait
  is the interrupt and closes at once. Rejected: a linger helper that each
  publisher holds and sleeps on, because every publisher would repeat the
  hold, and an origin-held linger, because it ties the session to what the
  origin holds.
- Tracks without a max-age add nothing to the wait. The alternative,
  `cache_duration`, defaults to `MAX` and would linger forever.

JS has no drop, so pick the interrupt idiom: a `close()` elsewhere already
ends the wait in Rust, so it may be enough; an `AbortSignal` is the other
option. Recommend one and report the API shape before landing.

Decide in `@moq/publish` which ends are clean (the source stops) and which
are abrupt (the element is removed, the user disables publishing).

Test with a mocked clock, as the Rust tests do: a late subscriber within
max-age gets the media, and the broadcast closes once it passes.
