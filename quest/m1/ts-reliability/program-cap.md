# [XS] TS import bounds the programs a PAT can create

## Goal

`moq import ts --program all` creates a broadcast only for a program the input actually describes, instead of one per program number a hostile PAT names.

Today `rs/moq-mux/src/container/ts/programs.rs` loops over every `pat.program_numbers()` and creates a broadcast, catalog producer, importer, and consumer for each. A sub-megabyte transport stream can list all 65,536 program numbers across PAT sections, so one input creates tens of thousands of live broadcasts.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Create a program's broadcast only once its PMT declares a supported stream, so the state scales with the PMT bytes the input carried, not with the PAT's list. The session's announce budget (100,000 by default, above the `u16` domain) does not bind on its own; it stays the bound a TS input shares with any publisher, and an operator-selected program list narrows it. `--program all` is opt-in; the default already refuses multi-program input.
- No new program-count constant: a cap below the PAT's domain would only refuse valid multiplexes.
- When the announce budget refuses a program, fail the import with the existing multi-program error shape, naming the count and the bound.
- Test with a synthesized PAT naming far more programs than it carries PMTs for, and assert only the described programs exist.

Public API: one new error case. Wire: none.

## Related

- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - the TS import contract this belongs to
- [PMT lifecycle](/quest/m1/ts-reliability/pat-lifecycle.md) - the PAT handling beside it
