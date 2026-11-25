# [L] The TS export subscribes to only the tracks it's asked for

## Goal

`moq export ts --track <filter>` exports, and subscribes at the relay to,
only the elementary streams its filters match, by kind, ISO 639 language,
PID or track name, so a hand-off that wants one audio language, or no video,
stops carrying the whole multiplex. The PMT and PCR PID follow from the
selection, and a selective export is unpadded unless `--mux-rate N` pads it.
Non-goals: `--passthrough`, whose one track of whole packets leaves nothing
to drop on the path, and selection in moq-srt's egress.

## Plan

Reported in [moq-dev/moq#9533](https://github.com/moq-dev/moq/issues/9533).
Today `export ts` reads every track and refuses the shared selection flags
(`ExportSink::ignores_selection` in `rs/moq-cli/src/args.rs`; `doc/bin/cli.md`).
Those flags pick one rendition per medium, so they couldn't name two audio
languages, teletext or SCTE-35 anyway.

Decided 2026-11-24 (maintainer, /quest-import):

- ✅ A TS-specific repeatable `--track <filter>` (rejected: extending the
  shared `SelectArgs` with languages and data modes, which pick one rendition
  per medium and give PID and SCTE-35 no meaning in other sinks). Filters
  such as `video`, `audio:lang=eng`, `scte35`, `pid=0x1f5`, `name=<track>`. A
  track is kept when any filter matches, and no `--track` keeps everything.
  Program-level SI (SDT, EIT, TDT and the other snapshot tracks) is always
  kept. moq-mux's TS export takes a typed selection, and the CLI parses into
  it. Pick the exact filter grammar, and how it shows in `--help`, in the PR.
- ✅ A selection exports unpadded unless `--mux-rate N` is given, decided from
  the flags alone, never the catalog, so the rate never flips mid-stream. A
  rate too low already fails, naming the rate it needs. Rejected: a per-track
  peak rate measured by the import and recorded in the catalog (a new
  `mpegts` draft field in Rust and JS, and a later video peak above it fails
  the export); requiring `--mux-rate` with every selection; padding to the
  source's rate, which spends the saving on null packets. This revisits
  [the export pacing quest](/quest/m3/ts-export-pace.md)'s rejection of an
  unpadded default tied to a selection, which rested on `enabled` toggles
  flipping the rate mid-stream. A command-line selection can't.
- ✅ The selection is a filter applied to every catalog update, so the PMT
  follows catalog changes as the unselected export does: a new matching track
  joins, a departed one leaves (rejected: resolving it once at start and
  ending the export when a selected track leaves). A selection that matches
  no media track in the first catalog fails.
- ✅ CLI and library only (rejected: also parsing a selection from moq-srt's
  streamid now). moq-srt can add one on the same library API when someone
  asks.

Placed in m3 by the planner, since a downstream hand-off waits on it.
The PCR stays where the export already places it: the first selected video,
else audio, else a PCR-only PID. Test that unselected tracks are never
subscribed at the relay, not just left out of the output. The reporter
offered to try a build on a hand-off to a receiving site. Ask them on the PR,
but don't wait on it to land.

Public API: moq-mux's TS export gains a track selection, and `moq export ts`
gains `--track`. Wire: none.

## Required

- [The TS export releases its multiplex on its PCR](/quest/m3/ts-export-pace.md) - makes `--mux-rate 0` mean unpadded, and edits the same rate and schedule code

## Closes

- [#9533](https://github.com/moq-dev/moq/issues/9533) - export ts: let a subscriber choose which tracks it exports
