# [XS] HLS import logs redact URLs

## Goal

No HLS import log line carries an unredacted playlist or segment URL. The
"selected master playlist renditions" info line (`rs/moq-hls/src/import.rs`)
prints the audio playlist URL as given, so a signed query string reaches the
logs. [moq-dev/moq#6767](https://github.com/moq-dev/moq/pull/6767) redacted the
error paths only.

## Plan

Route every URL the import logs through the redaction #6767 added, and sweep
moq-hls for other `tracing` calls that format a URL. A test captures the log
line and asserts the query string is gone.

Public API: none. Wire: none.
