# [XS] Half and quarter rate AC-3

## Goal

`moq-mux` parses AC-3 with bsid 9 and 10, the half and quarter sample-rate
variants, through TS and FLV import and TS export, instead of refusing them.

## Plan

Decided 2026-10-22 (maintainer, from the release audit): support them rather
than only documenting the refusal. It is cheap and removes an inconsistency
noted in [moq-dev/moq#5758](https://github.com/moq-dev/moq/pull/5758):
`ac3_core` in `rs/moq-mux/src/container/ts/export.rs` counts bsid up to 10 as an AC-3
core, then hands it to `ac3::parse_header`, which refuses anything above 8.

In `rs/moq-mux/src/codec/ac3.rs`, keep the frame-size tables and shift the
sample rate right by `bsid - 8`. Add a bsid 9 or 10 fixture through TS import
and TS export. Broadcast specs require 48 kHz, so these streams are rare, and
this stays low priority.

Public API: none. Wire: none.
