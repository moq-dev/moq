# [S] moq-transcode's idle-rung cancel test survives load

## Goal

`rung::tests::cancelling_an_idle_rung_reports_cancel` passes reliably under
host load, fixed at the cause rather than retried.

## Plan

Seen 2026-11-07 on [moq-dev/moq#8106](https://github.com/moq-dev/moq/pull/8106)'s
Check ([job](https://github.com/moq-dev/moq/actions/runs/42330347079/job/128275210492)),
a PR that doesn't touch moq-transcode: the test panicked at
`rs/moq-transcode/src/rung.rs:1077` because `serve` returned `Ok(())` before
the abort landed. It passed 5/5 unloaded. Reproduce under load first (pin it
to a few cores beside busy loops), then decide whether the race is in the test
or in how `serve` reports a cancel of an idle rung.
