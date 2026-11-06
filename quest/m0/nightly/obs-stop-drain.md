# [S] The OBS stop-drain test reads no gap

## Goal

Nightly's `Nightly (OBS)` and `Nightly (OBS TSan)` jobs pass the
`stop drains the session` case of `cpp/obs/test/moq-output-test.cpp`: after
`Stop()`, a subscriber reads every frame the session delivered and then the
track's end, with no gap. Find the root cause and fix it there, not by
loosening the assertion.

## Plan

Added 2026-11-05 (maintainer, /quest-audit), from scheduled Nightly
[run 42050683248](https://github.com/moq-dev/moq/actions/runs/42050683248).
Both [OBS](https://github.com/moq-dev/moq/actions/runs/42050683248/job/127325639496)
and [OBS TSan](https://github.com/moq-dev/moq/actions/runs/42050683248/job/127325639711)
failed the same check:

```text
FAIL test/moq-output-test.cpp:909: !std::holds_alternative<moq::MediaRead::kGap>((*result)->get_variant())
```

The 2026-11-04 scheduled run
([41852517544](https://github.com/moq-dev/moq/actions/runs/41852517544/job/126652295686))
failed the same line; the 2026-11-03 run
([41656613559](https://github.com/moq-dev/moq/actions/runs/41656613559))
passed both jobs, so start with what landed on `main` between them. The test
writes a 2 MiB keyframe just before `Stop()` and reads after the relay sees
the session close, so a gap means the relay or the container consumer reports
a group the session did not finish delivering, or the drain cut the large
frame off.

Public API: none expected. Wire: none expected.
