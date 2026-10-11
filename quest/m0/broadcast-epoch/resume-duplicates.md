# [M] Resume never delivers a late group twice

## Goal

An unordered reader receives each group once across same-epoch route
changes, even when a duplicate arrives very late. A genuinely unseen late
group remains eligible within the reader's bounds and budget.

## Plan

Reproduced in the 2026-10-10 audit: read group 0 from A, switch to B, read
groups 1 through 1024, then deliver B's delayed copy of group 0. The reader
receives group 0 twice. `model/resume.rs` forgets delivered IDs at
`MAX_DELIVERED = 1024` without proving those groups can no longer arrive.

Decided: this data-correctness fix gates m0. Preserve both duplicate
suppression and unordered delivery; advancing a floor past every late
group would hide the duplication by losing valid data. Bound bookkeeping
using the actual reader/copy lifetime, retained ranges, and terminal
dispositions rather than an arbitrary number of recently delivered IDs.
Choose the representation during implementation; do not retain one entry
per delivered group for the lifetime of an endless broadcast.

Land the failing regression, a genuinely unseen late group beyond the old
window, repeated route flaps, and ordered-reader controls. Cover readers
that keep old groups open and change their group bounds, and prove finished
copies release their bookkeeping. Tests use mocked time and existing CI
lanes. Extend the resume benchmark over readers and retained/reordered
groups so memory and delivery costs are measured.

Public API: duplicate suppression is corrected; no signature changes.
Wire: none. Update stale resume comments inline. JS handover must preserve
the corrected contract instead of copying the fixed-size history.

## Related

- [Resume reorder](/quest/m1/resume-reorder.md) - an unseen reordered group is not mistaken for a missing one
- [JS track handover](/quest/m1/js-group-handover.md) - the same delivery contract in JS
