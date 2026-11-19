# [S] A TS import ends on a packet header it cannot parse

## Goal

moq-mux's TS import ends with the cause on a TS adaptation field or PES header
that arrives intact but does not parse, instead of counting it as `damaged`
and resuming at the next PES. Loss (a continuity break) and packets the
receiver flagged with the transport error indicator keep today's handling:
both are understood source behavior, detected where they happen.

## Plan

Found by [moq-dev/moq#9170](https://github.com/moq-dev/moq/pull/9170), which
made an undecodable media unit end the import and left these packet-level
paths alone because its quest's Goal named only `Fault` and `unit_error`.
The same rule applies (maintainer, 2026-11-18): "We should not try to support
random bit flips that we don't understand and limp along with broken
functionality." Over UDP and SRT a packet arrives whole or not at all, and RF
corruption sets the transport error indicator, so a malformed header with
neither is a muxer bug or an unexplained flip.

In `rs/moq-mux/src/container/ts/import.rs`, `Import::damage` is reached for
`malformed TS adaptation field` (`route` and `pes_packet`), a `PesStart`
parse or length failure (`pes_packet`, `pes_header`), and `scrambled PES is
not supported` (`handle_pes_start`). Recommended: the first two end the
import; truncation by loss, a header cut short at a PMT revision or end of
input, and the transport error indicator stay as damage. Scrambled PES is a
known source (conditional access) and likely belongs with `refuse` rather
than `damage`; decide it here with a test that names the source.

Tests: `damaged_video_recovers`' `pes`, `adaptation`, and `adaptation-clock`
cases, `invalid_adaptation_lengths_damage_media_and_dedicated_clock_pids`,
`damaged_adaptation_on_a_dedicated_pcr_pid_keeps_the_clock`,
`damaged_pes_start_flushes_the_unit_before_it`, and
`malformed_pes_prefix_is_refused_without_waiting_for_more_bytes` become
"ends with the cause". `doc/bin/ts.md` and `doc/bin/srt.md` already say a
whole unit that does not decode ends the import; extend them to headers.

Public API: none. Behavior: a TS ingest with a malformed packet header ends
instead of continuing. Wire: none.
