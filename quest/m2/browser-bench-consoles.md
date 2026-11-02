# [XS] The browser bench keeps its page consoles

## Goal

A `bench/browser` run directory keeps the publisher's and consumer's page
console logs beside `report.json`, so a binding's error reason survives the run
and a failed nightly cell can be diagnosed from its artifacts.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Follow-up from [moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135),
whose diagnosis was slowed by the missing page logs.

The other half of that follow-up, a relay that says why a relayed track ended,
is already owned: [moq-dev/moq#7396](https://github.com/moq-dev/moq/pull/7396)
made a relay abort of a malformed track send PUBLISH_DONE `MALFORMED_TRACK`
instead of `INTERNAL_ERROR`, and
[PUBLISH_DONE says why a subscription ended](/quest/m2/publish-done-codes.md)
covers the remaining codes.
