# [S] Pinned consumers adopt the CLI flag names

## Goal

Downstream deployments that pin moq at a commit before
[moq-dev/moq#6522](https://github.com/moq-dev/moq/pull/6522) move their pin to
a commit that includes it and use its spellings, so the next release cut does
not break their invocations. Done when no pinned consumer passes a spelling
that #6522 renamed or removed.

## Plan

Decided 2026-10-23: the upstream rename landed on its own, and this migration
was split out because it can only follow a pin bump.

Move each pin and rewrite its invocations in the same change. The older pins
only understand the released retry spellings, so renaming first breaks the
binary they run. Upstream keeps no compatibility aliases; released spellings
fail at startup and name their replacement.

The `## Unreleased` entries in the `MIGRATE.md` of moq-cli, moq-gst, and
`@moq/publish` are the full list. Search each consumer for:

- `--backoff-*`, `--goaway-*`, `MOQ_BACKOFF_*`, `MOQ_GOAWAY_*`: add the
  `connect-` / `CONNECT_` prefix.
- `MOQ_LISTEN_TLS_*` on `moq export hls`: `MOQ_HLS_TLS_*`.
- `--passthrough` on `import fmp4` or `import hls`: `--cmaf`. TS keeps
  `--passthrough`.
- `--max-delay` on `export rtmp`: `--delay`.
- `moqsrc versions=`: `connect-version=`. `moqsink linger` and the
  `<moq-publish>` `linger` attribute are gone; clean completion always serves
  the finished tail.
