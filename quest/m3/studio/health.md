# [M] Stream health

## Goal

`moq-studio` shows how its stream is actually delivered, per track: viewers,
the bandwidth granted, what viewers report receiving and dropping, and
end-to-end latency, rather than only encoder counters. A problem names its
cause in plain words (the uplink is congested, a source stalled, the encoder
is behind).

## Plan

Read what the stack already publishes: the catalog's `stats` and `echo`
entries and `hang::{stats,echo}::Snapshot` (`doc/concept/stats.md`), and the
relay's stats broadcasts. OBS reports "0 dropped frames" while the bitrate sits
at zero, which is the confusion to avoid.

## Related

- [Stats](/quest/m2/stats/README.md) - the stats tracks this reads
- [Latency ledger](/quest/m2/latency-ledger.md) - per-stage playback delay a viewer reports
