# [XS] TS import bounds the programs a PAT can create

## Goal

`moq import ts --program all` creates a broadcast only for a program the input actually describes, instead of one per program number a hostile PAT names.

Today `rs/moq-mux/src/container/ts/programs.rs` loops over every `pat.program_numbers()` and creates a broadcast, catalog producer, importer, and consumer for each. A sub-megabyte transport stream can list all 65,536 program numbers across PAT sections, so one input creates tens of thousands of live broadcasts.

## Plan

Decided 2026-10-22 from the security audit, with the maintainer:

- Create a program's broadcast only once its PMT declares a supported stream, so the state scales with the PMT bytes the input carried, not with the PAT's list. `--program all` is the operator's opt-in to every described program, and an operator-selected program list narrows it; the default already refuses multi-program input.
- No new program-count constant or run budget: a cap below the PAT's domain would only refuse valid multiplexes. The session announce budget does not apply here, since it counts what a remote peer announces to us, not broadcasts `Programs` creates locally.
- Test with a synthesized PAT naming far more programs than it carries PMTs for, and assert only the described programs exist.

This starts after [PAT changes](/quest/m1/ts-reliability/pat-lifecycle.md) lands, since that quest rewrites `programs.rs`.

Public API: none. Wire: none.

## Required

- [PAT changes](/quest/m1/ts-reliability/pat-lifecycle.md) - rewrites `programs.rs`, the PAT handling this changes

## Related

- [TS fidelity and recovery](/quest/m1/ts-reliability/README.md) - the TS import contract this belongs to
