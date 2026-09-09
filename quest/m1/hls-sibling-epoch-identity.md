# [S] A replaced sibling publisher restarts its rendition

## Goal

An HLS export never serves a replacement sibling publisher's restarted groups
under timeline rows produced for the previous publisher. When the sibling
broadcast is replaced, the exporter drops that rendition's rows, re-binds, and
lists only rows produced after.

## Plan

`moq_mux::Source::bind` (`rs/moq-mux/src/source.rs:185-187`) starts one request
at rendition construction and retains its result, so the sibling a rendition
serves is the one the origin resolved. No epoch is needed to tell a
replacement apart: a bound sibling whose publisher is replaced by a different
first hop already ends with `Error::Dropped`
(`rs/moq-net/src/model/origin.rs:2852-2866`, test
`different_first_hop_ends_the_subscription` :4902). A same-hop replacement
re-splices at a group boundary invisibly to subscribers (`origin.rs:3663-3668`)
and needs nothing from the exporter.

On that end, the rendition clears its window, binds the sibling again through
`Source::bind`, and admits only timeline rows that arrive after the new bind
resolved. Rows listed for the old publisher are never served from the new one;
a segment request for one answers 404.

Add a deterministic regression with a held catalog and timeline, a bound
sibling, and a replacement through a different first hop after timeline rows
arrive. Also cover a replacement already present when the export starts: it is
the sibling the request resolves to and serves normally.

## Related

- [HLS cache misses](/quest/m1/hls-cache-miss-codes.md) - classify a missing group once the bound publisher is known
