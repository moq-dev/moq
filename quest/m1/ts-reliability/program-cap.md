# [XS] TS import bounds the programs a PAT can create

## Goal

`moq import ts --program all` creates a broadcast per PAT program only up to a bound the run already has, instead of one per `u16` a hostile multiplex can name.

Today `rs/moq-mux/src/container/ts/programs.rs` loops over every `pat.program_numbers()` and creates a broadcast, catalog producer, importer, and consumer for each. A sub-megabyte transport stream can list all 65,536 program numbers across PAT sections, so one input creates tens of thousands of live broadcasts.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Bound the program count by the origin's announce budget, or by an operator-selected program list, rather than a new arbitrary constant. `--program all` is opt-in; the default already refuses multi-program input.
- Fail the import with the existing multi-program error shape, naming the count and the bound.
- Test with a synthesized PAT naming far more programs than a real multiplex.

Public API: one new error case. Wire: none.

## Related

- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - the TS import contract this belongs to
- [PMT lifecycle](/quest/m1/ts-reliability/pat-lifecycle.md) - the PAT handling beside it
