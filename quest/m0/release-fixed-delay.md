# [XS] A fixed latency on release ignores the catalog jitter and delay

## Goal

The release deploy on `moq.dev` includes the fixed-delay backport. A viewer
who picks a fixed latency (100 ms, 250 ms, ...) plays at that delay even as
the rendition's catalog delay and jitter grow. Confirm the deployed revision
contains the fix and numeric selections no longer include those estimates.

## Plan

The implementation merged into release in
[moq-dev/moq#6217](https://github.com/moq-dev/moq/pull/6217). Verify its release
deploy reaches moq.dev, then retire this quest; no duplicate backport is
needed.

The backport preserves the behavior from
[moq-dev/moq#4162](https://github.com/moq-dev/moq/pull/4162) without its
breaking watch API rework: a numeric configured delay is used directly;
"auto" and "instant" remain unchanged. The release-side tests and live-browser
validation are recorded in the merged PR. Public API and wire are unchanged.
