# [M] TS export carries 48 kHz ATSC AC-3 at every A/52 rate

## Goal

`moq export ts` carries 48 kHz AC-3 as ATSC (`stream_type` 0x81, with the
2,592-byte main buffer of ATSC A/53 Part 5) at every A/52 rate up to 640 kb/s
without missing a decode deadline, and the output passes `compliance.py`'s strict
`tstd` check. Today every rate from 384 kb/s up aborts the export, and 384 and
448 kb/s 5.1 is ATSC's standard main audio. The carriage stays ATSC: DVB AC-3
(5,696 B) already fits every rate, and E-AC-3 has
[its own quest](/quest/m2/ts-eac3.md). 48 kHz is the only rate ATSC carries.
A 44.1 or 32 kHz frame that fits the buffer goes out like any other, and one
that outgrows it on its own (2,786 and 3,840 B at 640 kb/s) fails the export
with an error naming both sizes, rather than the misleading "raise the delay or
the multiplex rate" or a switch to DVB carriage.

## Plan

Decided (2026-10-10), planned from #5207, whose multi-PID `--headroom` arm had
to drop its AC-3 to 192 kb/s. `Schedule::admit`
(`rs/moq-mux/src/container/ts/schedule.rs`) keeps a unit's bytes in its
decoder buffer until the 25 ms slot after its due slot is past, not until it
decodes. AC-3 frames are 32 ms apart, so two frames' due slots can be one
apart, and the second must then fit beside the whole first: at 384 kb/s the
two hold 3,312 B against 2,592. The T-STD (ISO 13818-1 2.4.2.3) frees B at the
decode instant, and a 448 kb/s frame takes about 7 ms at Rx = 2 Mb/s, leaving
some 25 ms of its 32 ms spacing. The B overflow (122 %) graded on that run's
partial output is the same coarse timing, not a separate bug.

- Free each unit's bytes at its decode instant, for every PID with a decoder
  buffer (AC-3, AAC, MP2, E-AC-3, and video's EB), so the schedule follows the
  standard everywhere rather than special-casing AC-3. In the slot where a
  buffer frees up, the packets admitted against the freed room go after that
  instant: `Slot::layout` (from #5207) gains an earliest position per PID
  beside its round-robin. Chosen over admitting by sub-slots (the same idea,
  coarser) and over shrinking the 25 ms PCR grid, which would cost each PID
  about a tenth of its Rx to the per-slot slack, 2.5 times the clock packets,
  and every PCR-timing expectation.
- A unit bigger than its PID's decoder buffer can never be admitted, so the
  export fails it when it is pushed, naming the unit's size and the buffer's,
  rather than waiting for its deadline to pass. Once every source has ended,
  the tail still goes out late, as today. Chosen over switching such a track
  to DVB carriage, which would silently change what an ATSC receiver is handed.
- [Send-ahead](/quest/m1/tstd/send-ahead.md) bounds a unit's reach by what its
  decoder buffer holds and changes the same code; whichever lands second
  rebases onto the other.
- Test: schedule unit tests at 384, 448, 512, and 640 kb/s frame sizes against
  the 2,592 B buffer, each frame finishing by its deadline, with the existing
  decoder-buffer tests moved to the exact decode instant, and a 32 kHz
  448 kb/s frame (2,688 B) failing with both sizes named. Raise the
  `--headroom` arm's AC-3 (`test/ts/run.sh`) from 192 to 640 kb/s, the worst
  case, so CI grades it under strict `tstd` on every run.
