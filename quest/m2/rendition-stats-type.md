# [XS] Publish reports rendition stats as hang's type

## Goal

`@moq/publish`'s `Rendition.stats` takes hang's rendition stats schema's input
type (`z.input` of the schema behind `Stats.Rendition`), so publish no longer
keeps its own `RenditionStats` shape (`js/publish/src/rendition-internal.ts`)
beside the schema it writes. The encoders keep writing plain numbers into it
(`effect.proxy` in `js/publish/src/{video,audio}/encoder.ts`).

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088)): `Report.Writer`
(`js/hang/src/report.ts`) accepts the schema's input type (`z.input`) and
brands on validation, so publishers pass plain numbers while readers keep the
`u53Schema` guarantee. The producer-side `Rendition.stats` signal carries that
same input type, since the encoders write it directly, not through
`Report.Writer`. Not chosen: dropping the `u53` brand from the stats
schemas, which would lose the 53-bit integer check on read.

It changes a published TypeScript type, so it lands before the cut.
