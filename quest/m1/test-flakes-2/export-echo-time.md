# [S] The moq-cli echo report test runs on mocked time

## Goal

`report::export_echo_reports_where_the_import_solicits`
(`rs/moq-cli/tests/integration/report.rs:384`) finishes in well under a second
of wall time, so it stops being flagged SLOW in every `just check`.

## Plan

Six quest-spawn agents on 2026-10-25 saw it take about 35 s under load and
flagged it as SLOW. Find what it waits on in wall time and drive that from a
mocked clock or an event, per the root rule that unit tests mock time. If part
of it must stay real-time, move that part to Nightly.

Public API: none. Wire: none.
