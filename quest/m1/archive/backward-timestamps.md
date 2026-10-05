# [XS] Archive refuses backward timestamps

## Goal

A resumed recording refuses a track whose first group's timestamp is before
the recovered track's last recorded timestamp, failing loud like the existing
group-ID check, instead of writing overlapping media time. The caller starts a
new prefix.

## Plan

Check at enrollment against the recovered timeline, and test both a backward
and a forward restart. The 2026-09-30 audit folded this into the recording
writer, which landed without it. The archive format may break in place since
it is unreleased.

## Related

- [Per-track timelines](/quest/m1/archive/track-timeline/README.md) - reshapes the recovery this check reads
