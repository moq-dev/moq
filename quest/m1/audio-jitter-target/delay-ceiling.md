# [S] One exported 10 s playout ceiling

## Goal

The 10 s ceiling on playout delay, latency and catalog `warmup` is one
exported constant in Rust, mirrored by name in JS, and every other copy reuses
it. Today it is four constants under three names, kept equal by a test in one
case and by comments in the rest:

- `@moq/watch`'s `DELAY_MAX` (`js/watch/src/sync.ts`, internal to the package).
- `moq_audio::playback::Input::LATENCY_MAX` (public, `rs/moq-audio/src/playback/sink.rs`).
- moq-cli's private `DELAY_MAX` (`rs/moq-cli/src/play/args.rs`), duplicated
  because that module builds without the `play` feature and so without
  moq-audio; a test pins it to `LATENCY_MAX`.
- `moq_mux::container::WARMUP_MAX` (public, added by
  [moq-dev/moq#8549](https://github.com/moq-dev/moq/pull/8549)).

## Plan

Decided 2026-11-12 by the maintainer (coordinator session): ✅ unify the
ceiling, and keep clamping an oversized catalog `warmup` rather than rejecting
the rendition (rejected: leave the four constants as merged; reject an
oversized warmup).

Recommendation: define it in `moq_mux::container`, which moq-cli always
depends on and moq-audio already imports for `WARMUP_MAX`, and mirror it in
`@moq/hang`'s container module (`js/hang/src/container`), which mirrors
`moq_mux::container`. Pick one name for what it bounds (playout delay), such
as `DELAY_MAX`, replacing `WARMUP_MAX` and `Input::LATENCY_MAX`, and have
`@moq/watch` import it. Removing two public Rust constants is breaking: mark
the title with `!` and give the migration in the PR's Impact section.

Public API: one constant replaces `moq_mux::container::WARMUP_MAX` and
`moq_audio::playback::Input::LATENCY_MAX`, plus its JS export. Wire: none.
