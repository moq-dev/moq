# [M] MSF feedback tracks

## Goal

`--stats` and `--echo` work under `--catalog-format msf`: a soliciting
catalog's `echo` maps onto an MSF `publishTracks` entry (draft-ietf-moq-msf
§5.1.5), and a viewer publishes the hang `.echo` snapshot there. Start once a
consumer asks.

## Plan

Raised in [#5360](https://github.com/moq-dev/moq/issues/5360). Until then the
[media stats schema](/doc/concept/stats.md#media-reports) leaves both sections out of
the MSF catalog. The [Rust reporters](/quest/m2/stats/rust.md) quest refuses a
viewer's `--echo` under `--catalog-format msf` when it adds that flag
(decided 2026-10-12).

Open: how the entry names the hang payload, since MSF fixes its packaging
values in the draft; and whether to add a CMCD v2 Event Mode encoding
(draft-wilaw-moq-cmcd-event-timeline) of the overlapping fields (stalls,
dropped frames, latency, throughput, errors) for consumers that ingest CMCD.
