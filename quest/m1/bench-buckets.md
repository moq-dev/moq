# [S] Backport moq-bench's mergeable interval buckets

## Goal

The `release` line carries main's mergeable interval latency histograms, so
load harnesses can combine several bench processes over whole report
intervals. Preserve the version-1 sparse millisecond buckets, interval
bounds, documentation, and regression tests already landed on `main`.

## Plan

[#5268](https://github.com/moq-dev/moq/pull/5268) landed as `0a47269be`.
Decided in the quest audit: only its release backport remains; do not reopen
the histogram layout decision. Backport that commit in a separate PR to
`release`, including [#5021](https://github.com/moq-dev/moq/pull/5021)'s interval
delta if it is still absent there. Downstream load harnesses track `release`,
which is why the maintainer requested this backport on 2026-10-05.

Run the landed histogram merge and interval tests on the backport and keep
the same JSONL contract: merging is exact at bucket resolution, a partial
interval cannot be split, and publisher/subscriber measurements share one
host clock. Preserve the README's layout and clock documentation.

Public API: none (CLI output only). Wire: none.
