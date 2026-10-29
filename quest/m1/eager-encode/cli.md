# [S] moq-cli capture encodes always unless --on-demand

## Goal

`moq publish` capture of a camera, display or microphone encodes from start
to finish by default. `--on-demand` opens and encodes only while subscribed,
as today.

## Plan

Decided 2026-10-28 (maintainer); see the [epic](/quest/m1/eager-encode/README.md).
`rs/moq-cli/src/publish.rs:592` calls both `publish_capture`s, so the default
follows the libraries. The flag selects their on-demand option
(`moq_video::encode::Capture::on_demand` and
`moq_audio::encode::Capture::on_demand`). The comment there and
`doc/bin/cli.md:250` describe the always-on camera; add the flag and the
microphone. Name the flag under the repository's CLI rule.

Tests: the CLI's capture args parse `--on-demand`, and the default publishes
without a subscriber.

Public API: CLI default change, plus a new flag. Wire: none.
