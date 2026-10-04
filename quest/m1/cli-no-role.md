# [XS] moq export and play can leave out a role

## Goal

`moq export` and `moq play` accept `--no-video` and `--no-audio`, so an
audio-only or video-only output needs no placeholder rendition name.

## Plan

`SelectArgs::selection` in `rs/moq-cli/src/subscribe.rs` always opts both
roles in, and `moq_mux::select` clears a role that is not opted in.

Decided (2026-10-04): spell the flags `--no-video` and `--no-audio`, matching
`moq publish` (`rs/moq-cli/src/publish.rs`), so the CLI keeps one spelling and
nothing is renamed. Each conflicts with its role's `--*-name` and `--*-codec`,
and both together are refused. When set, `selection()` skips that role. Update
`doc/bin/cli.md` and the `--help` examples.

Tests: parsing refuses the conflicts; `--no-audio` yields a selection with no
audio role.

## Closes

- [#4787](https://github.com/moq-dev/moq/issues/4787) - close this issue when the quest finishes
