# [L] The TS export releases its multiplex on its PCR

## Goal

`moq export ts`, and moq-srt's egress, release the rebuilt multiplex evenly
across each PCR interval, as `--passthrough` has since
[moq-dev/moq#9232](https://github.com/moq-dev/moq/pull/9232), so a sender
that sends as it reads (`tsp -O ip`, an SRT sender) needs no groomer: its
10 ms bursts stay near the passthrough's 1.17× of the mean. Through a hole in
the media, PCR-only slots keep leaving on time instead of up to 1 s of
backfilled slots going out at once when media resumes. `--mux-rate 0` exports
unpadded, as ffmpeg's `-muxrate` does. Non-goal: rendition selection for
`export ts`, which the CLI refuses.

## Plan

Reported in [moq-dev/moq#9476](https://github.com/moq-dev/moq/issues/9476),
which measured 2.53× bursts and a 26.6 ms gap at 25 Mb/s. Verified at
`888de61a12`: the export already releases each 25 ms slot when due
(`rs/moq-mux/src/container/ts/export.rs` `slot_due`, `lay_due`), but hands it
over whole. The CLI writes it in one piece (`rs/moq-cli/src/subscribe.rs:559`),
and moq-srt gives every chunk of a slot one send time
(`rs/moq-srt/src/server.rs:146-157`). That is the shape the passthrough had
before #9232. The issue's line "as early as the receiver's buffers admit"
(`doc/bin/ts.md:126`) describes where a frame sits in the multiplex, not when
it is written.

Decided 2026-11-23 (maintainer, /quest-import):

- ✅ Pace in the export's writer, with the hole case in the same quest
  (rejected: pacing only; leaving grooming downstream). The export owns the
  clock and the byte schedule, and its docs already promise release at the
  time its PCRs assert (`export.rs:4057`). Reuse the passthrough's `Spread`
  (`release.rs`): each slot spreads from its due time to the next slot's, and
  goes out whole when the next slot is not adjacent. Each PCR is its own
  byte's time at the rate, so spreading padded output is exact. Check that
  `shed`'s `caught_up` and the per-frame discontinuity counter still hold once
  a `Frame` is a run rather than a slot.
- ✅ Through a hole, lay PCR-only slots as time passes, rather than nothing
  (`export.rs:3961-3970`) followed by the backfill burst (`schedule.rs:72-75`,
  `1256-1264`; `ts.md:210-211`). That keeps TR 101 290's PCR repetition clean
  while media is missing, padded or not.
- ✅ `--mux-rate 0` means unpadded, following ffmpeg, where `-muxrate` ≤ 1 is
  VBR (rejected: rejecting 0; a separate `--unpadded` flag). It already
  exports unpadded (`export.rs:1692`), but warns "ignoring invalid". Document
  it in `ts.md` and the flag's help, and make every other invalid rate, from
  the flag or the catalog, a loud error.
- ✅ No unpadded default tied to a selection (rejected): `export ts` has no
  selection, and toggling a rendition's `enabled` would flip the rate
  mid-stream.

Test it with oracle-style checks like `an_object_is_spread_across_its_pcr_interval`.
Measure the CLI piped through `tsp --realtime -O ip` the way the issue did,
and report the burst ratio and largest gap. The issue's author offered to run
a build into a receiving site's broadcast monitor and MDI probe. Ask them on
the PR, but don't wait on it to land.

Public API: moq-mux's export yields runs rather than whole slots. `--mux-rate`
rejects invalid rates other than 0. Wire: none.

## Closes

- [#9476](https://github.com/moq-dev/moq/issues/9476) - the media-aware TS export needs a groomer before a UDP or SRT sender

## Related

- [TS SI tables repeat on the schedule's grid](/quest/m3/ts-si-grid.md) - edits the same wire-time schedule, so expect conflicts
- [moq-srt per-connection rate handle](/quest/m3/srt-rate-handle.md) - gains from a paced export
