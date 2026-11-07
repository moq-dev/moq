# [XS] Publish reports rendition stats as hang's type

## Goal

`@moq/publish`'s `Rendition.stats` is typed as hang's `Stats.Rendition`, so
publish no longer keeps its own `RenditionStats` shape
(`js/publish/src/rendition-internal.ts`) beside the schema it writes.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088)): `Report.Writer`
(`js/hang/src/report.ts`) accepts the schema's input type (`z.input`) and
brands on validation, so publishers pass plain numbers while readers keep the
`u53Schema` guarantee. Not chosen: dropping the `u53` brand from the stats
schemas, which would lose the 53-bit integer check on read.

It changes a published TypeScript type, so it lands before the cut.
