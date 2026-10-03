# [S] Plan: untimed verbatim PES

## Goal

Decide how a verbatim MPEG-TS track carries a PES that has no PTS without
inventing a time, from import through export. The usual case is asynchronous
private data such as KLV. The output is an implementation quest, or a
recorded reason to keep today's behaviour.

## Plan

Today the importer stamps a PES without a PTS at the track's live edge, or
at 0 before the first frame. `drafts/draft-lcurley-moq-mpegts.md` says 0.
Both are invented times, which
[moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) rules
out for moq-net itself.

Making the moq-net timestamp optional isn't enough on its own. Verbatim
tracks use the legacy container, whose payload always starts with a
timestamp, and its decoder reads the time from there, not from the frame. So
the decision covers:

- **Carriage:** either the legacy payload keeps a time, or verbatim tracks
  move to a container that takes its time from the moq-net frame. Weigh what
  the second costs existing catalogs and consumers of the `mpegts` section.
- **Export:** how the TS exporter schedules and writes an untimed PES, which
  goes out with no PTS.
- **Stream types:** which ones this applies to. A media elementary stream
  may split an access unit across PES packets or infer timing. There, a
  missing PTS doesn't mean untimed.

The maintainer decides the carriage, because it touches the catalog and
container contract.

Test plan for the implementation quest: importer to encoded track to
exporter, for a PES without a PTS and for one with a real PTS of 0.

## Required

- [moq-net carries untimed frames faithfully](/quest/m1/untimed-model.md) - a frame must be able to carry no timestamp
