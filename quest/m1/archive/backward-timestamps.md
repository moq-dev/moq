# [XS] Archive refuses backward timestamps

## Goal

A resumed recording refuses a track whose source restarted, failing loud
instead of recording nothing and then overlapping media time. The caller
starts a new prefix.

Both signs of a restart count: a first group at or below the recovered
track's committed tail, which the writer drops today with only a `debug` log,
and a first timestamp before the recovered track's last recorded timestamp.

## Plan

Check at enrollment against the recovered timeline, and test a restart from
group 0, a backward timestamp, and a forward restart that keeps recording.
Mind a writer restarted against a source that kept running: its first group
may be the partially recorded tail group, which resumes rather than refuses.
The 2026-09-30 audit folded this into the recording writer, which landed
without it; the [#4034 review](https://github.com/moq-dev/moq/pull/4034#issuecomment-6005694031)
found the silent group drop. The archive format may break in place since it
is unreleased.
