# [XS] moq-quic's all-features rustdoc builds

## Goal

`cargo doc -p moq-quic --all-features` succeeds; today it fails on
`SentPackets`, already broken on `main` before
[moq-dev/moq#6950](https://github.com/moq-dev/moq/pull/6950).

## Plan

Fix the doc build, and add the all-features rustdoc build to whichever lane
should have caught it.

Public API: none.
