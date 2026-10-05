# [S] moq export and play can leave out a role, and no sink ignores selection

## Goal

`moq export` and `moq play` accept `--no-video` and `--no-audio`, so an
audio-only or video-only output needs no placeholder rendition name. A sink
that cannot honor rendition selection refuses the selection flags instead of
silently ignoring them.

## Plan

`SelectArgs::selection` in `rs/moq-cli/src/subscribe.rs` always opts both
roles in, and `moq_mux::select` clears a role that is not opted in. `play`
flattens the same `SelectArgs`. `export hls`, `rtmp`, `srt`, and `rtc` ignore
every selection flag today (`rs/moq-cli/src/main.rs` reads `select` only for
stdout sinks).

Decided (2026-10-04):

- Spell the flags `--no-video` and `--no-audio`, matching `moq publish`
  (`rs/moq-cli/src/publish.rs`), so the CLI keeps one spelling. Each
  conflicts with its role's `--*-name` and `--*-codec`, and both together are
  refused.
- `export h264` and `export h265` refuse `--no-video`, which contradicts the
  format.
- Sinks that do not honor selection refuse every selection flag, old and new
  (fail loud).
- `play --no-video` plays audio with a blank, closable window, documented. A
  headless audio-only path is later work.
- Update `doc/bin/cli.md` and every example, checked against `--help`.

Tests: parsing refuses the conflicts; `--no-audio` yields a selection with no
audio role; `export hls --video-name x` is refused.

## Closes

- [#4787](https://github.com/moq-dev/moq/issues/4787) - close this issue when the quest finishes
