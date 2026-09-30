# [M] Importers publish stream timestamps; the catalog clock maps them to wall

## Goal

The fMP4, MPEG-TS, and FLV importers in `rs/moq-mux` have no `live()`: every
importer publishes the stream's own timestamps verbatim (after PTS unwrap),
and the catalog's root `clock` is what maps them to wall time. `moq import`
(`rs/moq-cli/src/publish.rs`) stops calling it. An encoder that restarts its
timestamps ends the broadcast with an error instead of being re-anchored
forward onto the old one; turning the republish into a new epoch is the
broadcast epoch line's outcome, not this quest's. The SRT, RTMP, and HLS gateways, which reuse these
importers, get the same behavior.

## Plan

Decided (2026-09-28, replacing the gateway live-clock quest, which planned
the opposite: every gateway opting into `live()`):

- Timestamps stay verbatim from the stream. Rewriting them onto an
  arrival-time anchor breaks same-hop importers, which must derive every
  timestamp from the input alone (the hop-aligned import quest in
  [#4388](https://github.com/moq-dev/moq/pull/4388)), and hides the source's
  own timeline from anything downstream.
- The catalog clock carries the mapping. An importer establishes the root
  `clock` so the stream's PTS converts to wall time (the first frame is live
  on arrival), rather than translating each timestamp. Open: whether the
  importer sets the catalog clock from its first frame (the mapping is fixed
  at construction today, `moq_mux::Clock` and `catalog::Config::with_clock`)
  or the caller builds the catalog once the first PTS is known. Every track of
  one input, and every rendition of one HLS import, shares that one mapping.
- An encoder restart (a PTS rewind or a signalled time-base discontinuity) is
  a new epoch, not a forward re-anchor: the importer ends the broadcast with
  an error, and the caller republishes, which the broadcast epoch line turns
  into a fresh `@<uuidv7>`. Until that line lands, a restart fails loud.
- Delete `live()` from `ts::Import`, `fmp4::Import`, and `flv::Import`, the
  crate-private `clock::Anchor`, and `SourceMap` if nothing else uses it.
  fMP4 passthrough stops rewriting `tfdt`. A published `moq-mux` API break,
  so this targets `dev`.

Tests: per importer, a source starting at a large PTS publishes that PTS and
a catalog clock that maps it to near the arrival time; a rewind ends the
broadcast with an error. Update `doc/lib/rs/moq-mux.md` (the `live()`
paragraph), `doc/bin/cli.md`, and the gateway pages under `doc/bin/`, and
replace `ts_import_publishes_on_the_broadcast_clock` in moq-cli.

## Related

- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - the new epoch an encoder restart becomes
- [TS import shared shift](/quest/m1/ts-import-shared-shift.md) - the TS re-anchor shift that must stay input-derived
