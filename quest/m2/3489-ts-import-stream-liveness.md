# [M] moq import ts: every elementary stream reports its access units and how long it has been quiet

## Goal

An operator polling `moq import ts` can tell that a specific PID stopped
delivering access units while the mux kept flowing. `Import::stats` carries a
row for every elementary stream, video and verbatim included, with the
published access-unit count and the current gap since the last one, measured
on the transport's own clock. No threshold and no warning: a sparse stream
such as SCTE-35 is legitimately quiet, so liveness is derived by whoever
alarms. Nothing changes in what is published or how tracks are mapped.

## Plan

Measured in #3489: a 60 s video outage behind a running mux passes every TR
101 290 P1 check, produces identical logs, and delivers a 57 s hole; only the
audio half is visible, late, through #3372's resync line.

- `rs/moq-mux/src/container/ts/import.rs`: `Stream::stats` returns `None`
  for `H264`, `H265`, `Opus`, `Verbatim`, and `Clock`, and `Stats` documents
  that an absent PID is healthy. Give every elementary stream a row and
  retire the absence contract; the `lost.then_some(stats)` gate goes with it.
  `StreamStats` is `#[non_exhaustive]`, so the new fields are additive.
- Clock: the packet loop reads the adaptation field for the discontinuity
  indicator and skips the PCR bytes; parse the PCR there. `last_pts` advances
  on video PES only and freezes with the very stream this must catch. Record
  the clock at each access unit in `flush`; the gap is the current clock
  minus that mark. Land the adaptation-field parse once, shared with
  [TS timebase discontinuity](/quest/m0/ts-forward-discontinuity.md).
- Surface: `rs/moq-cli/src/publish.rs` `log_stats` gains a line for a stream
  whose count stopped advancing across a sample, distinct from the audio
  resync message. The SRT gateway reports nothing today; that surface is
  [SRT import stats](/quest/m2/srt-import-stats.md).
- Name and shape the counters so the TR 101 290 quest adopts them as its
  `PID_error` check, and leave the catalog `stalled` bit alone; that is the
  ladder and publisher-stats work.
- Tests with the issue's stimulus shape: suppress one PID's PES while keeping
  its PCR and continuity legal, assert the row's count stops and the gap
  grows; audio and SCTE-35 arms.

## Closes

- [#3489](https://github.com/moq-dev/moq/issues/3489) - close this issue when the quest finishes

## Related

- [SRT import stats](/quest/m2/srt-import-stats.md) - the same rows read from the SRT gateway
- [#1838](/quest/m3/1838-tr-101-290-monitoring-requirements-broadcast-contribution.md) - the monitoring model that subsumes this as `PID_error`
- [Publisher stats](/quest/m2/qos/publisher-stats.md) - where per-rendition liveness would ride the catalog
- [TS timebase discontinuity](/quest/m0/ts-forward-discontinuity.md) - shares the adaptation-field parse
