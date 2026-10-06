# [XS] An export failure is not the broadcast ending

## Goal

`moq export ts --linger` keeps waiting for a live broadcast when the export
itself fails, instead of treating that failure as the broadcast ending and
starting the linger countdown (`rs/moq-cli/src/subscribe.rs`).

## Plan

Reported by t0ms while grading #4645, who offered to do the CLI side. Tell
an export error apart from a finished broadcast: the error is reported and
fails the run as before, and only a real end starts the linger. A test drives
each case.

Public API: none. Wire: none.
