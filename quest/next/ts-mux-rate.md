# [S] moq ts: record the source mux rate and pad export to it

## Goal

`moq import ts` records the transport stream's constant rate in the catalog,
and `moq export ts` pads its output with null packets to that rate, so a
broadcast that came in as a CBR multiplex leaves as one an IRD or groomer can
accept without measuring anything. The rate is the whole multiplex, every PID
plus PSI plus stuffing, which is what PCR clocks; per-rendition `bitrate`
keeps its codec meaning and is not touched. Import of a VBR or unpaced source
records nothing, and export of a catalog without the field is byte-identical to
today.

Non-goals: MSFTS catalog convergence, carrying source PCR, SI coverage, and
group alignment stay in [#3731](https://github.com/moq-dev/moq/issues/3731)
until the next MSFTS revision is published; this quest settles only decision 6
there.

## Plan

- `rs/moq-mux/src/container/ts/catalog.rs`: add `mux_rate: Option<u64>` to
  `Mpegts` (serialized `muxRate`, bits per second), mark it with
  `#[serde(default, skip_serializing_if = "Option::is_none")]`, and include it
  in `is_empty`. Document it as the rate the PCR clock paces the multiplex at,
  not a sum of elementary streams. An absent rate stays omitted from the
  serialized catalog rather than becoming `null`.
- Import (`rs/moq-mux/src/container/ts/import.rs`): the PCR PID already parses
  its adaptation field. Between two PCRs on that PID, the rate is
  `packets * 188 * 8 / (delta PCR / 27 MHz)`; count every packet of the stream
  including null PID `0x1fff`, which the routing gate currently drops before
  counting. Publish the rate once it is stable across a window (a few seconds
  of PCR intervals within a small tolerance) and republish the catalog only
  when the stable value moves more than 1 % from the published one, so
  measurement noise never churns the catalog. Model measurement as collecting
  or published: a full window of disagreeing intervals transitions published
  back to collecting, republishes the catalog once with `muxRate` omitted, and
  discards the window; a PCR discontinuity does the same immediately. Publish
  again only after a fresh stable window. The 1 % threshold applies only to
  stable-to-stable updates, not clearing an invalid rate. A VBR or file-paced
  source therefore leaves the field absent.
- Export (`rs/moq-mux/src/container/ts/export.rs`): the exporter already emits
  one `Frame` per 25 ms PCR grid slot. When `mux_rate` is present, maintain a
  signed fixed-point packet balance: add the exact fractional allowance
  `mux_rate * 25 ms / (188 * 8)` each slot, subtract every emitted media, PSI,
  and SI packet, then emit and subtract `floor(max(balance, 0))` null packets
  (PID `0x1fff`, no adaptation, payload of `0xff`). Retain the fractional
  remainder across slots instead of rounding each slot independently. A slot
  that already exceeds its allowance emits no nulls and carries the negative
  balance forward so the long-run rate holds; media is never delayed or dropped
  to fit, so a source that sustains more than the recorded rate simply overruns
  it, and the exporter logs once per overrun run rather than growing the debt
  without bound (cap it at one second of packets). Pad only when the field is
  present. Add `Export::with_mux_rate(u64) -> Self` as an explicit override of
  the catalog value; `moq export ts --mux-rate <bps>` calls it when provided,
  including for a catalog without the field.
- Docs: `doc/bin/cli.md` gains the flag and a sentence on padding. Document the
  field in the `rs/moq-mux/src/container/ts/catalog.rs` module docs; `mpegts` is
  a `moq-mux` application extension and is not part of the Hang draft.
- Tests: an import fixture with a known CBR rate and stuffing (the existing
  `test_data` sources, or a synthesized one) yields the expected `muxRate`
  within tolerance; a VBR fixture yields none; a transition test covers
  published rate to instability or discontinuity, omitted field, and a newly
  stable rate; export with the field emits a stream whose measured rate matches
  and whose PCR intervals stay under 40 ms; a non-integral packet rate such as
  1,000,000 bps verifies the cumulative packet count and retained fractional
  remainder over many slots; the builder and CLI override beat a catalog value
  and supply an absent one; export without a field or override is unchanged.

Public API: one additive field on the `Mpegts` catalog section, one
`Export::with_mux_rate` builder, and one CLI flag. Wire: an additive catalog
field; no draft change, the `mpegts` section is an application extension.

## Related

- [#3731](https://github.com/moq-dev/moq/issues/3731) - decision 6 of six; the issue stays open for the MSFTS convergence questions

- [#2779](/quest/next/2779-moq-export-ts-continuity-counters-are-numbered-from.md) - the other determinism gap in the same exporter
- [TR 101 290](/quest/future/1838-tr-101-290-monitoring-requirements-broadcast-contribution.md) - the monitoring that would grade the padded output
