# [M] moq export ts releases frames on a fixed delay

## Goal

`moq export ts --delay <dur>` works like an SRT receiver's TSBPD. Each frame
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
- Late frames are dropped, as SRT's too-late drop does. A dropped video frame
  leaves its track waiting for the next keyframe.

Open: a backlogged or finished source (a file sink, an archive replay) would
come out at real-time pace from the first frame's arrival. Decide whether
release only gates drops and ordering, with pacing left to the CLI's
`Delivery` pacer, or whether file sinks skip it.

Test with mocked time: two exporters fed the same frames with different
arrival skew and loss emit identical packet order, and a frame arriving after
its deadline is dropped. Rerun the #4613 netem rig (10% loss, 120 s) and
compare with #4618's numbers.

Update `doc/bin/cli.md` and the `moq export ts` examples.

Public API: `ts::Export` takes the delay in place of its max age and loses the
hold; breaking, on `dev`. Wire:
none.

## Related

- [TS byte schedule](/quest/m1/ts-export-byte-schedule.md) - uses this delay as its mux-ahead buffer delay
- [Plan: max-delay](/quest/m1/plan-max-delay.md) - whether `max_age` becomes `max_delay` everywhere else
