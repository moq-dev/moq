# [XS] moq-cli import finishes its catalog producer

## Goal

No moq-cli import path drops a catalog `track::Producer` without `finish()`
or `abort()`. The warning seen in about 1 of 7 loaded runs of
`import_delivers_the_catalog_finish_at_eof` is traced to its producer and
fixed, or shown harmless and the warning's cause removed.

## Plan

Found in #5155, decided 2026-10-09: under load the test sometimes logs
`track::Producer dropped without finish() or abort() track=catalog.json`,
while the subscriber still sees a clean catalog finish. So some second
catalog producer, or a clone, is dropped on a path that skips `finish()`.
Find which one and finish or abort it at its source.

Public API: none. Wire: none.
