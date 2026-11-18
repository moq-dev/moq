# [L] A time lookup is a group fill

## Goal

A track's request handler serves one kind of fill. A reader that starts or
ends at a time no cached group resolves asks for the groups covering that
time, as an ordinary group fill, so `track::Fill`, `Fill::Lookup`, and
`FillLookup` are gone from moq-net, js/net, moq-ffi, and libmoq, and no
handler skips lookups by hand.

## Plan

Decided 2026-11-18 (maintainer, /quest-plan): ✅ fold lookups into group fills,
"we should have one mechanism" (rejected: lookups as an opt-in second kind;
leaving it). Groups are addressed by their start timestamp on lite-07, so a
time range maps onto groups: a reader wanting timestamps 40-60 may be filled
with the groups spanning 30-70, from the group holding 40 through the one
holding 60.

Today a reader's window that cannot resolve a time start asks a fill with
`time: Some(t)` (`look_up` in `rs/moq-net/src/model/track.rs`, `Ask::time`
in `track/fill.rs`), which surfaces as `Fill::Lookup`. Only moq-archive
answers it, from its index; libmoq, moq-ffi, moq-transcode, and js/net's lite
and ietf subscribers drop it, and js/net notes "the wire fetches by group
alone, so a lookup goes unanswered and its reader falls back".

Design the fill so a handler resolves a time-bounded start the way it
resolves any start: moq-archive from its index, an upstream subscription by
asking its peer, any other handler by dropping groups it does not have.
Check first whether a lite-07 SUBSCRIBE or FETCH can carry a time-bounded
start (Range Anchors compare times against starts), so a relay forwards it
instead of dropping it; record what the wire allows in this Plan before
building. If the wire cannot carry it, the relay drops such a fill as today,
and the API still has one kind.

Public API: breaking in moq-net, js/net, moq-ffi, and libmoq; handlers lose
their lookup arm. Wire: none expected; a wire change would follow the
lite-07 rules.
