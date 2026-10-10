# [XS] A TS adaptation field must fit its packet

## Goal

`adaptation_valid` (`rs/moq-mux/src/container/ts/import.rs`) refuses an
adaptation-only packet (`adaptation_field_control == 0b10`) whose adaptation
field length is not 183, and a packet with both (`0b11`) whose field is longer
than 182, as ISO 13818-1 requires, so either is treated as damage like any
other malformed adaptation field. Lands on `main` first, then is
cherry-picked to `release`.

## Plan

Found reviewing #5124. Add a regression test covering both boundaries,
on a media PID and on the PCR PID.

Public API: none. Wire: none.
