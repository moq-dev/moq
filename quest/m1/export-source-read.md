# [XS] ExportSource reads gaps and ends as Read items

## Goal

`moq_mux::container::ExportSource` (`rs/moq-mux/src/container/source.rs`)
consumes the container read's `Read::{Frame, Gap, End}` items instead of
polling the consumer's crate-private `discontinuity()` and `markers()`
counters, and those counters are deleted.

## Plan

#5288 made gaps and declared ends items in the read stream but left
`ExportSource` on the old counters, so the exporters (fMP4, TS, MKV, FLV, and
the raw H.264/H.265 exports) still learn of a gap after the fact.

- Map `Gap` to the exporter's discontinuity handling and `End` to its
  declared-end handling, in delivery order.
- Delete `Consumer::discontinuity()` and `Consumer::markers()` once nothing
  reads them, along with `ExportSource::markers()` if it only forwards.
- Existing exporter tests must pass unchanged; add one where a gap and an end
  arrive back to back to pin the order.

Public API: none (crate-private). Wire: none.
