# [S] Plan: a generation identity for cacheable media URLs

## Goal

A settled answer to where a generation ID comes from, so
[HLS generation](/quest/m2/hls-generation.md) can start. The broadcast epoch
was going to supply it;
[moq#3225](https://github.com/moq-dev/moq/pull/3225) removed Epoch from the
drafts as spec-only and never implemented, retired
`draft-lcurley-moq-broadcast`, and made announcements prefix routes with no
per-broadcast identity to hang a generation on.

## Plan

Run /plan-quest with the maintainer. The question is narrow: a CDN edge needs
one URL to mean the same bytes on every edge, so the ID has to be minted by
the publisher and travel with the content, not derived from wall-clock or a
relay-local counter.

Candidate sources to settle between: a per-broadcast value returned on resolve
(`TRACK_INFO`), a publisher-declared field in the hang catalog, or something
the archive line already keys recordings by. Also settle the mid-broadcast
rendition reconfigure hole that
[HLS generation](/quest/m2/hls-generation.md) records, since a generation that
only moves on restart cannot version a live reconfigure either way.
