# [XS] An adaptation-only TS packet must fill its payload

## Goal

`adaptation_valid` (`rs/moq-mux/src/container/ts/import.rs`) refuses an
adaptation-only packet (`adaptation_field_control == 0b10`) whose adaptation
field length is not 183, as ISO 13818-1 requires, so it is treated as damage
like any other malformed adaptation field. Landed on `main`, then
cherry-picked to `release`.

## Plan

Found reviewing #5124. Add a regression test with a short adaptation-only
packet on a media PID and on the PCR PID.

Public API: none. Wire: none.
