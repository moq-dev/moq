# [M] Muxing exports release frames on a fixed delay

## Goal

`moq export ts --delay <dur>` works like an SRT receiver's TSBPD, through a
release stage in moq-mux that the FLV and MKV exports adopt next. Each frame
goes out at its media time plus a fixed delay, measured from when the first
frame arrived, and in `(pts, pid)` order across tracks. A frame that misses its
deadline is dropped and counted. Output order is then a function of the media
alone, even under sustained loss, and the output runs at the source's pace
instead of bursting after a stall.

## Plan

Decided (2026-09-30):

- Why: TS assumes a constant end-to-end delay. Its tracks share one PCR clock,
  and a receiver's T-STD buffers are sized for access units that arrive a fixed
  time before their DTS, which is what SRT and UDP sinks deliver.
- The release stage is shared, not TS-specific: it takes each track's
  frames, releases them in `(pts, track)` order at anchor + pts + delay, and
  drops and counts late ones. TS adopts it here; FLV and MKV follow in their
  own quests. fMP4 stays out: it deliberately skips cross-track ordering,
  because its demuxers index by track (`fmp4/export.rs`). The fixed delay is
  needed only at the final receiver; relays stay hop-by-hop.
  Open for the PR: the stage's name and module. Ask the maintainer.
- One mode. Fixed-delay release replaces the stall/hold interleave in
  `rs/moq-mux/src/container/ts/export.rs` (`stall`, `hold`, and the wait in
  `pick_next_track`), which is deleted. #4618 is the interim fix for #4613
  until this lands. The hold can't stay deterministic under loss, because a
  rewind clears every track's `timeline`, so the mux falls back to arrival
  order until a quiet track shows.
- The release clock is anchored at the first frame's local arrival. A rewind
  (a new program generation) re-anchors it. Publisher clock drift slowly
  grows or shrinks the buffer; correct it only if measured.
- The flag is `--delay`, like `moq play`'s: one knob sets both the release
  delay and the sources' staleness budget, and it replaces `--max-age` on
  `moq export ts` (`moq play` already dropped `--max-age` for the same reason).
  moq-srt egress passes its SRT latency (`rs/moq-srt/src/ts.rs`, which feeds
  it to the muxer as the skip budget today).
- Deadlines are on decode time, not PTS. Reordered video reaches the muxer in
  decode order (PTS 0, 120, 40, 80 ms), so a PTS deadline would either strand
  a B-frame behind its reference or send it first. Each frame's deadline is
  anchor + its authored DTS + delay, each track keeps its decode order, and
  tracks interleave by `(DTS, pid)`, which is also the order T-STD removes
  access units in.
- Late frames are dropped, as SRT's too-late drop does. A dropped video frame
  leaves its track waiting for the next keyframe.

Open: a backlogged or finished source (a file sink, an archive replay) would
come out at real-time pace from the first frame's arrival. Decide whether
release only gates drops and ordering, with pacing left to the CLI's
`Delivery` pacer, or whether file sinks skip it.

Test with mocked time: two exporters fed the same frames with different
arrival skew, every frame inside its deadline in both, emit identical packet
order. Separately, a frame arriving after its deadline is dropped and counted,
and the rest still go out in `(DTS, pid)` order. A B-frame fixture (decode
order PTS 0, 120, 40, 80) loses nothing on a clean path. It must pass before
the hold is deleted. Rerun the #4613 netem rig (10% loss, 120 s) and
compare with #4618's numbers.

Decided in the 2026-10-05 audit: TS export rewind folds in here, since
#4645's jitter generations already keep the clock through a skipped group and
flag a PCR break only on a declared restart. What it still owes is the
backwards-time check: timestamps restarting within one broadcast are a
publisher bug (a name always means the same content), so the export fails
with an error rather than rewinding. The only input timestamp is the frame's
PTS, which legally moves backwards in decode order with B-frames (0, 120, 40,
80 ms), and the authored DTS cannot show a reset since `author_dts` clamps
every backwards candidate to `prev + 1`. So the check runs on PTS before that
clamp and is group-aware: a frame whose PTS is below the largest PTS of the
track's previous group is a reset. Test: a source whose timestamps restart at
zero after 10 s fails the export, while a B-frame sequence and open-GOP
leading pictures are still accepted.

#4645 also completes [TS byte schedule](/quest/m1/tstd/byte-schedule.md);
delete both quests in it. It targets the retiring line branch, so retarget it
to `main` once the line (#4640) lands, and merge `main` in.

Update `doc/bin/cli.md` and the `moq export ts` examples.

Promote `tstd` in `test/ts/compliance.py` from shape to hard, so `just test
ts` fails a round-trip the T-STD model rejects; it reports only until then.

Public API: `ts::Export` takes the delay in place of its max age and loses the
hold; breaking. `Export::stats` returns `ts::stats::Export` (decided in the
2026-10-05 audit, matching [TS stats module](/quest/m1/ts-stats-module.md)),
not a new `ts::export::Stats`. Wire:
none.

## Closes

- [#4767](https://github.com/moq-dev/moq/issues/4767) - TS export rewinds its clock on every generation change

## Related

- [FLV export delay](/quest/m1/flv-export-delay.md) - adopts the release stage
- [MKV export delay](/quest/m1/mkv-export-delay.md) - adopts the release stage
- [TS byte schedule](/quest/m1/tstd/byte-schedule.md) - uses this delay as its mux-ahead buffer delay
- [Subscriber max-delay](/quest/m1/subscriber-max-delay.md) - subscriber staleness is renamed; publisher retention stays `max_age`
