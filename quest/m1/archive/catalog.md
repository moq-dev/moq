# [S] Archive catalog

## Goal

A HANG catalog can advertise a durable archive, its Window timeline, and its
stable replay identity through one root `archive` entry.

## Plan

Add `archive` as a distinct root capability, not an alias for the current live
`timeline`. The entry keeps the timeline track, timescale, maximum duration, and
wall-clock anchor, then adds the archive format version, stable replay URL, and
broadcast epoch. The catalog supplies the timeline track's name and
configuration; it has no reserved physical identity.

The entry promises that every range in the advertised timeline is durably
FETCHable. A broadcast with no archive store has no `archive` entry. The latest
catalog group still comes from ordinary SUBSCRIBE, and authorization for the
replay URL remains external so managed and customer-owned stores share one
format.

Catalog composition preserves a child's archive entry instead of synthesizing
one for a derivative. A catch-all replay route may expose the stable URL, but
the catalog epoch identifies the exact recording generation.

This entry is portable discovery while the catalog exists. It does not keep a
source catalog alive or enumerate offline recordings. Managed deployments
(moq.pro) use their durable recordings API after source teardown and can expose
a growing deep window through a separate live archive contribution downstream.

Keep the existing `timeline` entry during migration so publishers without a
store remain exportable. [Archive catalog cutover](/quest/m1/archive/cutover.md)
deletes it after every supported publisher can emit `archive`; do not make
either entry a compatibility alias for the other.

## Required

- [Archive timeline](/quest/m1/archive/timeline.md)

## Related

- [Catalog version binding](/quest/m1/archive/catalog-version.md) - explicit
  historic applicability is deliberately separate
