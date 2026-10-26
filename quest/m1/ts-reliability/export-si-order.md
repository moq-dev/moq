# [S] TS export leads with its PMT and PCR

## Goal

A TS export holds its first SI tables and media until the PMT and one PCR
interval are out, so an export-to-import hop keeps each table's first emission instead of
dropping it until the table repeats.

## Plan

Since [moq-dev/moq#6757](https://github.com/moq-dev/moq/pull/6757), TS import
admits SI only once a valid program-clock interval exists. The export writes
its first SI ahead of its PMT and PCRs, so the importer drops it. Order the
export's first output as PAT, PMT, a PCR interval, then SI. A PCR interval
here is what `si.rs` needs to place a section: one forward PCR step of at most
`MAX_INTERVAL`, not the importer's two-second catalog window. Test it with an
export-to-import round trip that sees every SI table's first emission.

The first media PES also waits for that PCR, so a reader of our own export
never sees media it cannot place.

Not planned (2026-10-25, maintainer): bounding SI aging against a source that
claims a huge mux rate through valid PCR steps. That needs a program-clock
floor per input byte, a new contract.

Public API: none. Wire: none.
