# [M] The JS bundles shed what isn't worth its weight

## Goal

Every shipped JS bundle drops what does not earn its bytes, and the rest of
the growth since 2026-10-22 is justified in the PR as feature growth, so the
30-day size check passes once its window moves past the growth.

## Plan

Found by the triage of Nightly run
[43447849289](https://github.com/moq-dev/moq/actions/runs/43447849289)
(2026-11-13) and [moq-dev/moq#8654](https://github.com/moq-dev/moq/pull/8654):
every JS bundle grew 11 to 15% over about 96 js/net, qmux, and clock
commits, with no single culprit (`js/net` brotli 136,880 to 157,282 bytes).
The 30-day check (10% limit) compares against the 10-22 report from 11-21 and
fails every night until about 12-13 unless the bundles shrink.

Decided 2026-11-13 (maintainer, /quest-plan): ✅ audit and shrink, then accept
the rest as feature growth (rejected: cut until every bundle is within 10%,
which may cost features; accept without an audit). Do not loosen the check.

Known item: `@moq/clock`'s test-only `Manual` clock ships in every bundle
(about 2.3 KB) because the package index re-exports it. Attribute the rest
with `sh/size/bundles.ts`.

## Related

- [Audit cost](/quest/m3/audit-cost.md) - the recurring audit that also covers bundle size
