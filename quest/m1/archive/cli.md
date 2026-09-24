# [M] Record from moq-cli

## Goal

`moq-cli` can record any broadcast it imports into an `object_store` prefix
with `moq_archive::Writer`, so a local or S3 recording needs no custom code.

## Plan

As the native application that owns its storage and track choices, `moq-cli`
attaches the writer to every import path and enrolls the resulting
`broadcast::Consumer` tracks. It reads its own catalog: video and audio
renditions enroll as pacing tracks, the catalog and any other track as
non-pacing, and renditions added later enroll as they appear. The writer stays
catalog-agnostic.

Open questions for the maintainer: the flag shape (a recording URL plus
prefix, retention window, and deletion grace), which `object_store` backends
to compile in (local filesystem at minimum, S3 behind a feature), and the
per-broadcast prefix for listeners that accept many broadcasts (RTMP, SRT).

Update `doc/bin/cli.md` and every example invocation.

## Closes

- [#2281](https://github.com/moq-dev/moq/issues/2281) - close this issue when the quest finishes
