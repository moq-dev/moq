# [S] A recording stalled on an open group reports what it waits for

## Goal

When a moq-hls recording waits on a listed segment whose group a still
connected publisher never closes, the wait is visible: the export's status
and a log line name the rendition, segment, and group it waits on. The wait
itself stays unbounded; nothing is cut or discarded on a timer.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399) (open; it owns
[the restart drain cutoff](/quest/m2/hls-restart-drain-cutoff.md)):

- A recording waiting on a listed segment whose group never closes:
  - Bound it with a timer.
  - ✅ Report, no timer. Reason: consistent with the 2026-10-24 decision in
    the drain cutoff quest, which rejected wall-clock bounds and discarding
    listed rows; a disconnecting publisher already errors the read.
  - Leave it as is.

This is a general stalled-publisher case, not only takeover: a replaced
instance's cut and a live recording hang the same way. After [moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399) lands,
find where a cursor waits on a listed row's media
(`rs/moq-hls/src/export/rendition.rs`, the read [moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399)'s Codex P2 pointed
at) and surface the wait through the existing status or stats the export
exposes, plus one `warn` per stalled segment once it has waited past the
segment's own listed duration (a report threshold, not a cut).

Test: a publisher that lists a segment then holds its group open shows the
segment and group in status and logs, and the recording completes normally
once the group closes.

Public API: possibly a status field; none if existing stats carry it.
Wire: none.

## Related

- [Finite drain for a replaced run's HLS recording](/quest/m2/hls-restart-drain-cutoff.md) - [moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399) lands the cut this reports on
- [A library recorder gets the takeover cut](/quest/m2/mux-source-cut.md) - the other [moq-dev/moq#7399](https://github.com/moq-dev/moq/pull/7399) follow-up
