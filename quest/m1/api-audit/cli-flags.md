# [S] CLI and gst flags

## Goal

One flag or property name means one thing across moq-cli subcommands and
the gst elements, and the same knob has the same name everywhere.

## Plan

Decided 2026-10-22 (API audit):

- `export rtmp --max-delay` and `export flv --delay` name the same paced
  release; use `--delay` on both (neither spelling was released). Delete the
  hidden refusal of `--max-delay` on flv/mkv (`args.rs`): only released
  spellings get refusals, and release had `--max-age` there.
- `--passthrough` means CMAF fragments on `import fmp4|hls` (#6250) and the
  raw TS multiplex on `import|export ts`. Rename the fmp4/hls one to
  `--container cmaf` or another distinct name, consistent with
  [container formats](/quest/m1/api-audit/container-format.md).
- `linger`: drop moqsink's new boolean `linger` property and
  `<moq-publish>`'s `linger` attribute, keeping today's close at the end.
  Revised 2026-10-23 (maintainer): don't make every publisher finish through
  moq-net's `Finish`; [broadcast lifecycle](/quest/m1/broadcast-lifecycle/README.md)
  deletes it and owns completion and retention.
- gst: moqsrc's `versions` (comma list) versus the CLI's repeatable
  `--connect-version`; mirror one name, give moqsink the same, and give both
  elements the same connection properties (moqsrc lacks
  `quic-idle-timeout` and `quic-keep-alive`). moq-tokio has both
  `client-version` and `connect-version`; keep one.
- `auth serve --key`/`--key-dir`/`--key-set` conflicts become clap
  conflicts, not a runtime check.
- `--backoff-*` and `--goaway-*` take the `connect-` prefix their TOML keys
  have; `export hls --listen-tls-*` stops sharing `MOQ_LISTEN_TLS_*` with the
  MoQ listener; keep rustdoc links out of `--help`.

Update `doc/bin/cli.md`, `doc/bin/gstreamer.md`, and every invocation in the
repo and moq.pro.

Public API: CLI and gst breaking. Wire: none.
