# [XS] A spent capture retry budget reports its error

## Goal

When an ended camera or microphone track uses up the `Retry` budget in
js/publish (`js/publish/src/source/{camera,microphone,retry}.ts`), `out.error`
is set to the last error instead of the capture failing silently. A
regression test covers it.

## Plan

An open failure that spends the budget already sets `out.error`. A track that
ends calls `Retry.failed()` from its `ended` handler, and nothing reports it
when that was the last attempt. The gap predates #4827.

Public API: none. Wire: none.
