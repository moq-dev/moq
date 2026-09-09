# [M] Archive catalog

## Goal

A HANG catalog advertises its segment index and any durable archive through
one root `archive` entry that replaces `timeline`: the timeline track, the
replay MoQ broadcast path, the object-store URL, and the format version.

## Plan

One breaking catalog change: the root `archive` entry subsumes `timeline`
(`rs/hang/src/catalog/root.rs:39-42`), so exactly one entry names the segment
index. It carries:

- the timeline fields as they stand in `rs/hang/src/catalog/timeline.rs:24-55`
  (`track`, `timescale`, `durationMax`, `wall`)
- the replay MoQ broadcast path the archive is served back from, if any
- the object-store URL the objects live under, if the publisher exposes one
- the format version from the [Recording section](/drafts/draft-lcurley-moq-hang.md#recording)

A live publisher without a store advertises `archive` with the timeline
fields alone, and the store-less HLS export
(`rs/moq-hls/src/export/mod.rs:3-8`) reads `archive.timeline` where it reads
`timeline` today, so nothing loses HLS across the change. The entry promises
that every range in the advertised timeline is FETCHable; with a store it
promises the ranges are durable. There is no alias, dual-write, or fallback
`timeline` entry, and no epoch: a wildcard replay path names no generation,
and a client that must tell recordings apart compares the entry's replay path
and store URL.

The catalog supplies the timeline track's name and configuration; the
timeline has no reserved physical identity. The latest catalog group still
comes from ordinary SUBSCRIBE, and authorization for the replay path and
store URL stays external so managed and customer-owned stores share one
format. Catalog composition preserves a child's `archive` entry instead of
synthesizing one for a derivative.

The entry is portable discovery while the catalog exists. It does not keep a
source catalog alive or enumerate offline recordings; managed deployments
(moq.pro) use their recordings API after source teardown.

Land it as one change across `rs/hang` (the catalog type, `moq-mux`
`Producer::section` at `rs/moq-mux/src/timeline.rs:907`, `moq-hls`), the
`js/hang` mirror (`js/hang/src/catalog/root.ts:27`,
`js/hang/src/container/timeline.ts:131`), and a new Catalog Section in the
draft's Timeline chapter (`drafts/draft-lcurley-moq-hang.md:560`) replacing
`timeline`.

## Related

- [Catalog version binding](/quest/m1/archive/catalog-version.md) - explicit historic applicability is deliberately separate
