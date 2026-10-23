# [M] HLS recorder in the CLI

## Goal

`moq ... export hls --output <dir>` writes the broadcast's playlists and
segments to a directory as they complete, using moq-hls's recorder cursors,
and exits with an error if the recording ends abruptly or misses a segment.
It works alone or alongside `--listen`, so one stage can serve and record at
once.

## Plan

Decided 2026-10-23 (maintainer interview):

- A flag beside `--listen`, not a positional target or an object-store URL.
- Recording alone binds no port: `--listen` defaults to `[::]:8089` today, so
  that default must not apply when `--output` is given without `--listen`.
- This is the cursors' first consumer outside moq-hls's tests.
- Update `doc/bin/cli.md` and `doc/bin/hls.md`, and every example that uses
  `export hls`.

## Required

- [HLS cursors error on every incomplete recording](/quest/m1/hls-teardown.md) - an abrupt end or a missed segment must surface as an error before the recorder relies on it
